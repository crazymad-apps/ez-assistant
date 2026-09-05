#![cfg(unix)]

mod support;

use std::{
    fs,
    sync::{Arc, Mutex},
};

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
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
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated Workspace");
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
        "title": "Resource prompt fixture", "workspace_id": workspace_id, "model_key": "fixture"
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

    let captured = requests.lock().expect("captured wire requests");
    let prompts: Vec<String> = captured
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
        // Fork may also request an automatic title; it is not an Agent System Prompt.
        .filter(|prompt| prompt.contains("<runtime_directories>"))
        .collect();
    assert_eq!(prompts.len(), 4, "one actual Agent request per Run");
    for prompt in &prompts {
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
    drop(captured);

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

fn submit(client: &mut Client, session_id: &str, key: &str) {
    let run_id = string(
        &client.runtime(
            "submit_input",
            json!({
                "session_id": session_id, "message": key, "variant": "build", "idempotency_key": key
            }),
        )["run"]["run_id"],
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
