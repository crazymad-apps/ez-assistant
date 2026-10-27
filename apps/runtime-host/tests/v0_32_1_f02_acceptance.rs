#![cfg(unix)]

mod support;

use std::path::Path;

use reqwest::blocking::{
    Client as HttpClient,
    multipart::{Form, Part},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use support::{Client, FakeProvider, HostProcess, compatibility_headers, write_config};

const ATTACHMENT: &[u8] = b"F02 independently retained attachment";

#[test]
fn repeated_forks_keep_children_and_files_after_source_deletion_restart_and_new_execution() {
    let home = support::test_directory();
    let provider = FakeProvider::start();
    write_config(
        home.path(),
        provider.endpoint(),
        "f02-isolated-provider-key",
    );
    let first_host = HostProcess::start(home.path());
    let mut client = first_host.connect();
    let source = client.runtime("create_session", json!({"title":"F02 joint source"}));
    let source_id = source["session"]["session_id"].as_str().unwrap();
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":source_id,"approval_mode":"auto"}),
    );
    let upload = HttpClient::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!(
            "{}/sessions/{source_id}/attachments",
            first_host.base_url()
        ))
        .bearer_auth(first_host.access_token())
        .headers(compatibility_headers())
        .multipart(Form::new().part("file", Part::bytes(ATTACHMENT).file_name("retained.txt")))
        .send()
        .unwrap();
    assert!(upload.status().is_success());
    let uploaded: Value = upload.json().unwrap();
    let run = client.submit_started_input(json!({
        "session_id":source_id,"message":"DELEGATE_PARALLEL_CASE","variant":"build",
        "attachment_ids":[uploaded["attachment"]["attachment_id"]],"idempotency_key":"source-input"
    }));
    client.wait_for_status(
        "F02 source children",
        source_id,
        run["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    let source_view = view(&mut client, source_id);
    assert_eq!(source_view["child_tasks"].as_array().unwrap().len(), 2);
    let source_items = source_view["conversation"]["items"].as_array().unwrap();
    let fork_point = source_items
        .iter()
        .rev()
        .find(|item| item["type"] == "assistant")
        .unwrap()["message_id"]
        .clone();
    let ids: Vec<_> = source_items
        .iter()
        .map(|item| item["message_id"].clone())
        .collect();
    let database = home.path().join("users/_personal/data/runtime.sqlite3");
    let ledger_before = ledger(&database);
    assert!(ledger_before.0 > 0);
    assert_controls_released(&database);

    let first = client.runtime("fork_session", json!({
        "session_id":source_id,"fork_point":fork_point,"expected_generation":source_view["conversation"]["generation"]
    }));
    let first_id = first["session"]["session_id"].as_str().unwrap();
    let first_view = view(&mut client, first_id);
    let second = client.runtime("fork_session", json!({
        "session_id":first_id,"fork_point":fork_point,"expected_generation":first_view["conversation"]["generation"]
    }));
    let survivor_id = second["session"]["session_id"].as_str().unwrap();
    let inherited = view(&mut client, survivor_id);
    assert_eq!(
        ledger(&database),
        ledger_before,
        "fork and projection do not add model calls or usage"
    );
    assert_eq!(inherited["runs"], json!([]));
    assert_eq!(
        inherited["conversation"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["message_id"].clone())
            .collect::<Vec<_>>(),
        ids
    );
    let attachment_id = inherited["attachments"][0]["attachment_id"]
        .as_str()
        .unwrap();
    assert_ne!(
        attachment_id,
        uploaded["attachment"]["attachment_id"].as_str().unwrap()
    );
    assert_child_owners(&mut client, survivor_id, &inherited);
    assert_attachment(&first_host, survivor_id, attachment_id);

    // 唯一写入目标为本用例新建的 SQLite／会话文件；删除只涉及本用例创建的两个来源。
    for id in [source_id, first_id] {
        let prepared = client.runtime("prepare_delete_session", json!({"session_id":id}));
        client.runtime(
            "delete_session",
            json!({"session_id":id,"confirmation_token":prepared["confirmation_token"]}),
        );
    }
    {
        let connection = readonly(&database);
        for id in [source_id, first_id] {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM sessions WHERE session_id=?1",
                        [id],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE session_id=?1",
                    [survivor_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM inputs WHERE session_id=?1",
                    [survivor_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(home.path());
    let mut client = second_host.connect();
    let recovered = view(&mut client, survivor_id);
    assert_eq!(recovered["conversation"], inherited["conversation"]);
    assert_eq!(recovered["child_tasks"], inherited["child_tasks"]);
    assert_child_owners(&mut client, survivor_id, &recovered);
    assert_attachment(&second_host, survivor_id, attachment_id);
    let new_run = client.submit_started_input(json!({
        "session_id":survivor_id,"message":"FIRST_CASE","variant":"build","idempotency_key":"new-local-input"
    }));
    client.wait_for_status(
        "F02 new local execution",
        survivor_id,
        new_run["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    let continued = view(&mut client, survivor_id);
    assert_eq!(continued["runs"].as_array().unwrap().len(), 1);
    assert_eq!(continued["runs"][0]["run_id"], new_run["run"]["run_id"]);
    assert_eq!(continued["child_tasks"], inherited["child_tasks"]);
    {
        let connection = readonly(&database);
        let counts: (i64,i64) = connection.query_row("SELECT COUNT(*), COALESCE(SUM(queued_message_json IS NOT NULL),0) FROM inputs WHERE session_id=?1", [survivor_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(
            counts,
            (1, 0),
            "only original idempotency identity remains after consuming the queue"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM model_request_records WHERE session_id=?1",
                    [survivor_id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "only the new local model request belongs to the surviving branch"
        );
    }
    assert_controls_released(&database);
    let after_execution = ledger(&database);
    assert!(after_execution.0 > 0);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(second_host.wait().status.success());

    let third_host = HostProcess::start(home.path());
    let mut client = third_host.connect();
    let final_view = view(&mut client, survivor_id);
    assert_eq!(final_view["conversation"], continued["conversation"]);
    assert_eq!(final_view["runs"], continued["runs"]);
    assert_eq!(
        ledger(&database),
        after_execution,
        "restart reads do not repeat model accounting"
    );
    assert_controls_released(&database);
    assert_attachment(&third_host, survivor_id, attachment_id);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(third_host.wait().status.success());
}

fn view(client: &mut Client, session: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session}))["snapshot"]["value"].clone()
}

fn assert_child_owners(client: &mut Client, session: &str, view: &Value) {
    for child in view["child_tasks"].as_array().unwrap() {
        assert_eq!(child["task"]["inherited"], true);
        assert_eq!(child["can_cancel"], false);
        assert_eq!(child["task"]["session_id"], session);
        let id = &child["task"]["child_task_id"];
        let detail = client.runtime(
            "get_child_task_view",
            json!({"session_id":session,"child_task_id":id}),
        );
        let conversation = &detail["snapshot"]["value"]["conversation"];
        assert_eq!(conversation["owner"]["session_id"], session);
        assert_eq!(conversation["owner"]["child_task_id"], *id);
        assert!(!conversation["items"].as_array().unwrap().is_empty());
    }
}

fn assert_attachment(host: &HostProcess, session: &str, attachment: &str) {
    let response = HttpClient::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "{}/sessions/{session}/attachments/{attachment}/download",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .headers(compatibility_headers())
        .send()
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.bytes().unwrap().as_ref(), ATTACHMENT);
}

fn readonly(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn ledger(path: &Path) -> (i64, i64) {
    readonly(path)
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(total_tokens),0) FROM model_request_records",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn assert_controls_released(path: &Path) {
    let connection = readonly(path);
    for table in [
        "runs",
        "child_tasks",
        "body_appends",
        "child_body_appends",
        "pending_tool_exchanges",
        "child_pending_tool_exchanges",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "completed control remains in {table}");
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='run_message_refs'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
