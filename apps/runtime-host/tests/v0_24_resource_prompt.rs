#![cfg(unix)]

mod support;

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use agent_model::SystemPromptSnapshot;
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

#[test]
fn model_wire_keeps_file_uri_rules_frozen_and_rebuilds_them_on_clear() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(capture_request))
            .with_state(requests.clone()),
    );
    let runtime_home = isolated_directory();
    let workspace = isolated_directory();
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "offline-resource-fixture",
    );
    let instructions = workspace.path().join("AGENTS.md");
    fs::write(&instructions, "RESOURCE_INSTRUCTIONS_ORIGINAL").expect("initial instructions");

    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let workspace_id = client.runtime(
        "register_workspace",
        json!({
            "label": "Resource prompt fixture",
            "primary_directory": workspace.path(),
            "additional_directories": []
        }),
    )["workspace"]["workspace_id"]
        .clone();
    let session_id = string(&client.runtime("create_session", json!({
        "title": "Resource prompt fixture", "workspace_id": workspace_id, "model_selection":null
    }))["session"]["session_id"]);
    submit(&mut client, &session_id, "new-session");

    fs::write(&instructions, "RESOURCE_INSTRUCTIONS_UPDATED").expect("updated instructions");
    submit(&mut client, &session_id, "continue-session");
    let conversation = client.conversation(&session_id);
    let fork_point = conversation["items"]
        .as_array()
        .expect("conversation items")
        .iter()
        .find(|item| item["type"] == "assistant")
        .expect("assistant message")["message_id"]
        .clone();
    let fork_id = string(
        &client.runtime(
            "fork_session",
            json!({
                "session_id": session_id, "fork_point": fork_point,
                "expected_generation": conversation["generation"]
            }),
        )["session"]["session_id"],
    );
    submit(&mut client, &fork_id, "fork-session");

    client.runtime(
        "clear_session",
        json!({
            "session_id": session_id, "operation_id": "clear-resource-fixture",
            "expected_generation": conversation["generation"]
        }),
    );
    submit(&mut client, &session_id, "cleared-session");

    let prompts = captured_prompts(&requests);
    assert_eq!(prompts.len(), 4, "one actual Agent request per Run");
    for prompt in &prompts {
        assert_eq!(
            prompt
                .matches("<runtime_mode>personal</runtime_mode>")
                .count(),
            1
        );
        assert!(prompt.contains("connected EZ Assistant Runtime Host"));
        assert!(
            prompt.find("<available-skills").expect("skill catalog")
                < prompt
                    .rfind("<runtime_directories>")
                    .expect("environment part")
        );
        assert_eq!(prompt.matches("<local_resource_presentation>").count(), 1);
        assert!(prompt.contains("file:///absolute/path/report.md"));
        assert!(prompt.contains("Percent-encode spaces and reserved characters"));
        assert!(prompt.contains("confirming it exists and is a regular file"));
    }
    assert_eq!(
        prompts[0], prompts[1],
        "continuing preserves the frozen System Prompt"
    );
    for prompt in &prompts[..3] {
        assert!(prompt.contains("RESOURCE_INSTRUCTIONS_ORIGINAL"));
        assert!(!prompt.contains("RESOURCE_INSTRUCTIONS_UPDATED"));
    }
    assert!(prompts[3].contains("RESOURCE_INSTRUCTIONS_UPDATED"));
    assert!(!prompts[3].contains("RESOURCE_INSTRUCTIONS_ORIGINAL"));
    assert!(prompts[0].contains(&format!("/{session_id}/private")));
    assert!(prompts[2].contains(&format!("/{fork_id}/private")));
    assert!(!prompts[2].contains(&format!("/{session_id}/private")));
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

#[test]
fn legacy_prompt_survives_restart_while_fork_and_clear_get_current_identity() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(capture_request))
            .with_state(requests.clone()),
    );
    let home = isolated_directory();
    write_config(
        home.path(),
        provider.endpoint(),
        "offline-legacy-prompt-fixture",
    );
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let id = string(
        &client.runtime(
            "create_session",
            json!({
                "title":"Legacy identity fixture", "model_selection":null
            }),
        )["session"]["session_id"],
    );
    submit(&mut client, &id, "before-restart");
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());

    // 只操作本测试刚创建并已停止的用户域库；与正式加载入口核对同一 sessions 字段。
    // 夹具可重建，不备份用户数据，也不为生产增加任何旧格式写入路径。
    let database = home.path().join("users/_personal/data/runtime.sqlite3");
    assert!(database.is_file());
    let mut connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("existing isolated database");
    let transaction = connection.transaction().expect("fixture transaction");
    let sessions_before: i64 = transaction
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .expect("exact initial session count including the controller");
    assert_eq!(
        transaction
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_id = ?1",
                [&id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let original: String = transaction
        .query_row(
            "SELECT system_prompt_json FROM sessions WHERE session_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .expect("frozen prompt used by the Host");
    let prompt: SystemPromptSnapshot = serde_json::from_str(&original).expect("frozen snapshot");
    let mut parts = prompt.into_parts();
    let last = parts.last_mut().expect("Host environment part");
    assert!(last.starts_with("<runtime_directories>"));
    assert!(last.contains("<runtime_mode>personal</runtime_mode>"));
    *last = last
        .lines()
        .filter(|line| {
            !line.contains("<runtime_mode>") && !line.contains("<runtime_execution_semantics>")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let legacy = serde_json::to_string(&SystemPromptSnapshot::new(parts)).expect("legacy snapshot");
    assert_eq!(
        transaction
            .execute(
                "UPDATE sessions SET system_prompt_json = ?1 WHERE session_id = ?2",
                params![legacy, id],
            )
            .expect("install one legacy fixture"),
        1
    );
    assert_eq!(
        transaction
            .query_row(
                "SELECT system_prompt_json FROM sessions WHERE session_id = ?1",
                [&id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        legacy
    );
    transaction.commit().expect("commit fixture");
    drop(connection);

    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    submit(&mut client, &id, "restored-legacy");
    let conversation = client.conversation(&id);
    let fork_point = conversation["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["type"] == "assistant")
        .expect("assistant")["message_id"]
        .clone();
    let fork_id = string(
        &client.runtime(
            "fork_session",
            json!({
                "session_id":id, "fork_point":fork_point,
                "expected_generation":conversation["generation"]
            }),
        )["session"]["session_id"],
    );
    submit(&mut client, &fork_id, "fork-legacy");
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("inspect fixture after restore and Fork");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        sessions_before + 1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT system_prompt_json FROM sessions WHERE session_id = ?1",
                [&id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        legacy,
        "restore and Fork preserve source bytes"
    );
    drop(connection);

    client.runtime(
        "clear_session",
        json!({
            "session_id":id, "operation_id":"clear-legacy-prompt",
            "expected_generation":conversation["generation"]
        }),
    );
    submit(&mut client, &id, "clear-legacy");
    let prompts = captured_prompts(&requests);
    assert_eq!(prompts.len(), 4, "actual Agent requests across restart");
    assert!(prompts[0].contains("<runtime_mode>personal</runtime_mode>"));
    let legacy: SystemPromptSnapshot = serde_json::from_str(&legacy).expect("legacy snapshot");
    assert_eq!(
        prompts[1],
        legacy.parts().join("\n\n"),
        "restored wire preserves the frozen snapshot"
    );
    assert!(!prompts[1].contains("<runtime_mode>"));
    for prompt in &prompts[2..] {
        assert!(prompt.contains("<runtime_mode>personal</runtime_mode>"));
        assert!(prompt.contains("connected EZ Assistant Runtime Host"));
    }
    assert!(prompts[2].contains(&format!("/{fork_id}/private")));
    assert!(!prompts[2].contains(&format!("/{id}/private")));
    assert!(prompts[3].contains(&format!("/{id}/private")));
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("inspect final isolated state");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        sessions_before + 1
    );
    let rebuilt: String = connection
        .query_row(
            "SELECT system_prompt_json FROM sessions WHERE session_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .expect("clear rebuilt the persisted source prompt");
    let rebuilt: SystemPromptSnapshot = serde_json::from_str(&rebuilt).expect("rebuilt snapshot");
    assert_eq!(rebuilt.parts().join("\n\n"), prompts[3]);
    assert!(
        rebuilt
            .parts()
            .last()
            .expect("directories")
            .contains("<runtime_mode>personal</runtime_mode>")
    );
}

fn isolated_directory() -> TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).expect("test root");
    tempfile::Builder::new()
        .prefix("f06-resource-prompt-")
        .tempdir_in(fs::canonicalize(root).expect("test root"))
        .expect("fresh isolated directory")
}

fn captured_prompts(requests: &Mutex<Vec<Value>>) -> Vec<String> {
    requests
        .lock()
        .expect("captured requests")
        .iter()
        .map(|request| {
            request["messages"]
                .as_array()
                .expect("Chat messages")
                .iter()
                .filter(|message| message["role"] == "system")
                .map(|message| message["content"].as_str().expect("system text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        // Fork 的自动标题请求不是 Agent System Prompt。
        .filter(|prompt| prompt.contains("<runtime_directories>"))
        .collect()
}

fn submit(client: &mut Client, session_id: &str, key: &str) {
    let run_id = string(
        &client.submit_started_input(json!({
            "session_id": session_id, "message": key, "variant": "build", "idempotency_key": key
        }))["run"]["run_id"],
    );
    client.wait_for_status(key, session_id, &run_id, &["completed"]);
}

fn string(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}

async fn capture_request(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let response_id = {
        let mut requests = requests.lock().expect("capture wire request");
        assert!(requests.len() < 8, "unexpected unbounded model requests");
        requests.push(body);
        format!("resource-fixture-{}", requests.len())
    };
    let events = [
        json!({"id":response_id,"model":"offline-model","choices":[{"index":0,"delta":{"role":"assistant","content":"Fixture answer"},"finish_reason":null}]}),
        json!({"id":response_id,"model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":2,"total_tokens":102}}),
    ];
    let response = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    (
        [("content-type", "text/event-stream")],
        format!("{response}data: [DONE]\n\n"),
    )
}
