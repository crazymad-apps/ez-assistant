//! 仅在项目隔离 Runtime Home 中验证首次物化 key 的只读恢复。
#![cfg(unix)]

mod support;

use std::{fs, path::Path};

use reqwest::blocking::{
    Client as HttpClient,
    multipart::{Form, Part},
};
use serde_json::{Value, json};
use support::{FakeProvider, HostProcess, write_config};

fn assert_stale_materialization(client: &HttpClient, host: &HostProcess, key: &str) {
    let response = client.post(format!("{}/commands",host.base_url())).bearer_auth(host.access_token())
        .json(&json!({"request_id":"stale-materialization-query","command":{"scope":"runtime","payload":{"type":"get_session_materialization","payload":{"idempotency_key":key}}}}))
        .send().unwrap();
    assert!(!response.status().is_success());
    assert_eq!(
        response.json::<Value>().unwrap()["error"]["code"],
        "snapshot_stale"
    );
}

#[test]
fn materialization_key_reads_committed_result_without_replaying_input_or_upload() {
    let runtime_test_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&runtime_test_root).expect("prepare isolated Runtime test root");
    let home = tempfile::tempdir_in(runtime_test_root).expect("isolated Runtime Home");
    let provider = FakeProvider::start();
    write_config(
        home.path(),
        provider.endpoint(),
        "materialization-recovery-fixture",
    );
    let first_host = HostProcess::start(home.path());
    let mut runtime = first_host.connect();
    let key = "materialization-query-fixture";
    let agent_name = "审阅助手";
    let saved = runtime.runtime(
        "save_personal_agent",
        json!({
            "expected_revision":null,
            "config":{"name":agent_name,"description":"Host 冻结验证",
                "prompt":"只按第一版规则回答。","skills":[],"mcp":[]}
        }),
    );
    assert!(
        runtime.runtime(
            "get_session_materialization",
            json!({"idempotency_key":key})
        )["materialization"]
            .is_null()
    );

    let manifest = json!({
        "idempotency_key":key,
        "variant":"build",
        "approval_mode":"auto",
        "message":"FIRST_CASE",
        "mode":"normal",
        "custom_agent":{"name":agent_name,"source":{"type":"personal"}},
        "attachments":[{"selection_key":"fixture-file","original_name":"fixture.txt","size_bytes":7}]
    });
    let client = HttpClient::builder()
        .default_headers(support::compatibility_headers())
        .no_proxy()
        .build()
        .expect("HTTP client");
    let result: Value = client
        .post(format!(
            "{}/session-materializations",
            first_host.base_url()
        ))
        .bearer_auth(first_host.access_token())
        .multipart(Form::new().text("manifest", manifest.to_string()).part(
            "fixture-file",
            Part::bytes(b"content".to_vec()).file_name("fixture.txt"),
        ))
        .send()
        .expect("send first materialization")
        .error_for_status()
        .expect("first materialization succeeds")
        .json()
        .expect("decode first result");
    let session_id = result["session"]["session_id"]
        .as_str()
        .expect("session id");
    assert!(result["run"].is_null());
    let claimed = runtime.wait_for_input_run(session_id, result["input_id"].as_str().unwrap());
    let run_id = claimed["run_id"].as_str().unwrap();
    runtime.wait_for_status(
        "initial materialization",
        session_id,
        run_id,
        &["completed"],
    );
    let recovered = runtime.runtime(
        "get_session_materialization",
        json!({"idempotency_key":key}),
    );
    assert_eq!(
        recovered["materialization"]["session"]["session_id"],
        session_id
    );
    assert_eq!(recovered["materialization"]["input_id"], result["input_id"]);
    assert_eq!(recovered["materialization"]["run"]["run_id"], run_id);
    assert_eq!(recovered["materialization"]["run"]["status"], "completed");
    assert_eq!(
        recovered["materialization"]["attachments"],
        result["attachments"]
    );
    assert_eq!(
        recovered["materialization"]["session"]["custom_agent"]["name"],
        agent_name
    );
    runtime.runtime(
        "save_personal_agent",
        json!({
            "expected_revision":saved["revision"],
            "config":{"name":agent_name,"description":"Host 冻结验证",
                "prompt":"第二版规则不改写会话。","skills":[],"mcp":[]}
        }),
    );
    assert_eq!(
        runtime.runtime("list_runs", json!({"session_id":session_id}))["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    runtime.runtime("shutdown_runtime", json!({}));
    drop(runtime);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(home.path());
    let mut runtime = second_host.connect();
    let after_restart = runtime.runtime(
        "get_session_materialization",
        json!({"idempotency_key":key}),
    );
    assert_eq!(
        after_restart["materialization"]["session"]["session_id"],
        session_id
    );
    assert_eq!(
        after_restart["materialization"]["input_id"],
        result["input_id"]
    );
    assert_eq!(after_restart["materialization"]["run"]["run_id"], run_id);
    assert_eq!(
        after_restart["materialization"]["session"]["custom_agent"]["name"],
        agent_name
    );
    let retried: Value = client
        .post(format!(
            "{}/session-materializations",
            second_host.base_url()
        ))
        .bearer_auth(second_host.access_token())
        .multipart(Form::new().text("manifest", manifest.to_string()).part(
            "fixture-file",
            Part::bytes(b"content".to_vec()).file_name("fixture.txt"),
        ))
        .send()
        .expect("retry materialization")
        .error_for_status()
        .expect("same key retry succeeds")
        .json()
        .expect("decode retry");
    assert_eq!(retried["session"]["session_id"], session_id);
    assert_eq!(retried["run"]["run_id"], run_id);
    assert_eq!(
        runtime.runtime("list_runs", json!({"session_id":session_id}))["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let conversation = runtime.conversation(session_id);
    runtime.runtime("clear_session", json!({"session_id":session_id,"operation_id":"materialization-clear","expected_generation":conversation["generation"]}));
    let later = runtime.submit_started_input(json!({"session_id":session_id,"message":"FIRST_CASE later input","mode":"normal","variant":"build","attachment_ids":[],"quotes":[]}));
    let later_run = later["run"]["run_id"].as_str().unwrap();
    runtime.wait_for_status(
        "later input after clear",
        session_id,
        later_run,
        &["completed"],
    );
    assert_stale_materialization(&client, &second_host, key);
    let stale_retry = client
        .post(format!(
            "{}/session-materializations",
            second_host.base_url()
        ))
        .bearer_auth(second_host.access_token())
        .multipart(Form::new().text("manifest", manifest.to_string()).part(
            "fixture-file",
            Part::bytes(b"content".to_vec()).file_name("fixture.txt"),
        ))
        .send()
        .unwrap();
    assert!(!stale_retry.status().is_success());
    assert_eq!(
        stale_retry.json::<Value>().unwrap()["error"]["code"],
        "snapshot_stale"
    );
    let runs = runtime.runtime("list_runs", json!({"session_id":session_id}));
    assert_eq!(runs["runs"].as_array().unwrap().len(), 1);
    assert_eq!(runs["runs"][0]["run_id"], later_run);
    runtime.runtime("shutdown_runtime", json!({}));
    drop(runtime);
    assert!(second_host.wait().status.success());
    let third_host = HostProcess::start(home.path());
    let mut runtime = third_host.connect();
    assert_stale_materialization(&client, &third_host, key);
    assert_eq!(
        runtime.runtime("list_runs", json!({"session_id":session_id})),
        runs
    );
    runtime.runtime("shutdown_runtime", json!({}));
    drop(runtime);
    assert!(third_host.wait().status.success());
}
