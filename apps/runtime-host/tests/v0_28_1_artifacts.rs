//! 正式 Host 的交付声明、受控读取、跨会话及重启验收；仅使用新建隔离 Home。
#![cfg(unix)]
mod support;

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
use support::{Client, FakeProvider, HostProcess, write_config};

static RESPONSE: AtomicU64 = AtomicU64::new(1);
async fn provider(Json(body): Json<Value>) -> impl axum::response::IntoResponse {
    let messages = body["messages"].as_array().unwrap();
    let last_user = messages.iter().rposition(|m| m["role"] == "user").unwrap();
    let content = messages[last_user]["content"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            messages[last_user]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|part| part["text"].as_str())
                .find(|text| text.starts_with("ARTIFACT "))
                .unwrap_or("")
                .to_owned()
        });
    let done = messages[last_user + 1..]
        .iter()
        .any(|m| m["role"] == "tool");
    let id = format!("artifact-{}", RESPONSE.fetch_add(1, Ordering::SeqCst));
    let (delta, finish) = if !done && content.starts_with("ARTIFACT ") {
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "declare_artifact")
        );
        let input: Value = serde_json::from_str(content.trim_start_matches("ARTIFACT ")).unwrap();
        (
            json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":"declare_artifact","arguments":input.to_string()}}]}),
            "tool_calls",
        )
    } else {
        (json!({"role":"assistant","content":"交付检查完成"}), "stop")
    };
    let first = json!({"id":id,"model":"offline-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let last = json!({"id":id,"model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":20,"completion_tokens":5,"total_tokens":25}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
    )
}

fn view(client: &mut Client, session: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session}))["snapshot"]["value"].clone()
}
fn submit(client: &mut Client, session: &str, input: Value) {
    let run = client.runtime("submit_input", json!({"session_id":session,"message":format!("ARTIFACT {input}"),"variant":"build","mode":"normal","attachment_ids":[],"quotes":[]}))["run"].clone();
    client.wait_for_status(
        "artifact declaration",
        session,
        run["run_id"].as_str().unwrap(),
        &["completed"],
    );
}
fn create(client: &mut Client, workspace: &str) -> String {
    client.runtime(
        "create_session",
        json!({"title":"产物验收","workspace_id":workspace}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn file_request(
    host: &HostProcess,
    session: &str,
    action: &str,
    locator: &Value,
) -> reqwest::blocking::Response {
    reqwest::blocking::Client::new()
        .post(format!(
            "{}/sessions/{session}/resource-files/{action}",
            host.base_url()
        ))
        .headers(support::compatibility_headers())
        .bearer_auth(host.access_token())
        .json(&json!({"locator":locator}))
        .send()
        .unwrap()
}

#[test]
fn artifacts_survive_restart_and_only_explicit_available_files_are_delivered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f02-artifacts-")
        .tempdir_in(&root)
        .unwrap();
    let runtime_home = home.path().join("runtime-home");
    fs::create_dir(&runtime_home).unwrap();
    let workspace_path = home.path().join("workspace");
    fs::create_dir(&workspace_path).unwrap();
    for (name, body) in [
        ("final.txt", "FINAL DELIVERY"),
        ("helper.py", "print('helper')"),
        ("cache.tmp", "temporary"),
    ] {
        fs::write(workspace_path.join(name), body).unwrap();
    }
    let provider =
        FakeProvider::with_router(Router::new().route("/v1/chat/completions", post(provider)));
    write_config(&runtime_home, provider.endpoint(), "artifact-fixture");
    let host = HostProcess::start(&runtime_home);
    let mut client = host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({"label":"F02","primary_directory":workspace_path,"additional_directories":[]}),
    )["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = create(&mut client, &workspace);
    let other = create(&mut client, &workspace);
    submit(
        &mut client,
        &session,
        json!({"action":"declare","path":"final.txt","title":"最终报告"}),
    );
    let initial = view(&mut client, &session);
    assert_eq!(
        initial["artifacts"].as_array().unwrap().len(),
        1,
        "helper and cache must not be inferred"
    );
    let locator = initial["artifacts"][0]["locator"].clone();
    assert!(initial["artifacts"][0]["source_run_id"].is_string());
    assert_eq!(view(&mut client, &other)["artifacts"], json!([]));
    assert_eq!(
        file_request(&host, &session, "stat", &locator)
            .json::<Value>()
            .unwrap()["size_bytes"],
        14
    );
    assert_eq!(
        file_request(&host, &session, "preview", &locator)
            .json::<Value>()
            .unwrap()["text"],
        "FINAL DELIVERY"
    );
    assert_eq!(
        file_request(&host, &session, "download", &locator)
            .text()
            .unwrap(),
        "FINAL DELIVERY"
    );
    submit(
        &mut client,
        &session,
        json!({"action":"withdraw","path":"final.txt"}),
    );
    assert_eq!(view(&mut client, &session)["artifacts"], json!([]));
    assert_eq!(
        fs::read_to_string(workspace_path.join("final.txt")).unwrap(),
        "FINAL DELIVERY"
    );
    submit(
        &mut client,
        &session,
        json!({"action":"declare","path":"final.txt","title":"重新交付"}),
    );
    submit(
        &mut client,
        &session,
        json!({"action":"declare","path":"./final.txt","title":"更新后的报告"}),
    );
    assert_eq!(
        view(&mut client, &session)["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        view(&mut client, &session)["artifacts"][0]["title"],
        "更新后的报告"
    );
    for path in ["missing.txt", "../outside.txt", "."] {
        submit(
            &mut client,
            &session,
            json!({"action":"declare","path":path,"title":"不可交付"}),
        );
        assert_eq!(
            view(&mut client, &session)["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    fs::write(home.path().join("outside.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(
        home.path().join("outside.txt"),
        workspace_path.join("escape.txt"),
    )
    .unwrap();
    submit(
        &mut client,
        &session,
        json!({"action":"declare","path":"escape.txt","title":"越界"}),
    );
    assert_eq!(
        view(&mut client, &session)["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let expected = view(&mut client, &session)["artifacts"].clone();
    drop(client);
    host.kill();
    let host = HostProcess::start(&runtime_home);
    let mut client = host.connect();
    assert_eq!(view(&mut client, &session)["artifacts"], expected);
    fs::remove_file(workspace_path.join("final.txt")).unwrap();
    assert!(
        !file_request(&host, &session, "stat", &locator)
            .status()
            .is_success()
    );
    assert!(
        !file_request(&host, &session, "download", &locator)
            .status()
            .is_success()
    );
    assert_eq!(
        view(&mut client, &session)["artifacts"],
        expected,
        "missing file retains its delivery identity"
    );
    submit(
        &mut client,
        &session,
        json!({"action":"withdraw","path":"final.txt"}),
    );
    submit(
        &mut client,
        &session,
        json!({"action":"withdraw","path":"final.txt"}),
    );
    assert_eq!(view(&mut client, &session)["artifacts"], json!([]));
    assert!(workspace_path.join("helper.py").exists());
    assert!(workspace_path.join("cache.tmp").exists());
    drop(client);
    host.kill();
    let host = HostProcess::start(&runtime_home);
    assert_eq!(view(&mut host.connect(), &session)["artifacts"], json!([]));
}
