#![cfg(unix)]

mod support;

use serde_json::json;
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_deepseek_responses_config, write_responses_config};

#[test]
fn formal_host_uses_the_explicit_responses_protocol_without_chat_fallback() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_responses_config(runtime_home.path(), provider.endpoint(), "offline-secret");

    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let status = client.runtime("get_config_status", json!({}));
    assert_eq!(status["status"]["state"], "ready");
    let models = client.runtime("list_models", json!({}));
    assert_eq!(models["models"][0]["protocol"], "openai_responses");

    let created = client.runtime(
        "create_session",
        json!({"title":"Responses offline", "model_key":"responses-fixture"}),
    );
    let session_id = created["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "RESPONSES_CASE",
            "variant": "build",
            "idempotency_key": "responses-offline"
        }),
    );
    let run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let run = client.wait_for_status("Responses run", &session_id, &run_id, &["completed"]);
    assert_eq!(run["text"], "responses offline answer");

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "RESPONSES_TOOL_CASE",
            "variant": "build",
            "idempotency_key": "responses-offline-tool"
        }),
    );
    let run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("tool run id")
        .to_owned();
    let run = client.wait_for_status("Responses tool run", &session_id, &run_id, &["completed"]);
    assert_eq!(run["text"], "responses tool answer");
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| {
                tool["tool_name"] == "list_pinned_memories" && tool["status"] == "completed"
            }),
        "Responses tool call did not complete: {run}"
    );

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

#[test]
fn opaque_reasoning_survives_formal_host_restart_and_replays_on_the_exact_route() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_deepseek_responses_config(runtime_home.path(), provider.endpoint(), "offline-secret");

    let first_host = HostProcess::start(runtime_home.path());
    let mut first = first_host.connect();
    let created = first.runtime(
        "create_session",
        json!({"title":"Responses opaque restart", "model_key":"deepseek-responses"}),
    );
    let session_id = created["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let submitted = first.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "RESPONSES_OPAQUE_CASE",
            "variant": "build",
            "idempotency_key": "responses-opaque-store"
        }),
    );
    let run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let run = first.wait_for_status(
        "Responses opaque store",
        &session_id,
        &run_id,
        &["completed"],
    );
    assert_eq!(run["text"], "responses opaque stored");
    let product = first.conversation(&session_id);
    let product_text = serde_json::to_string(&product).expect("serialize product projection");
    assert!(!product_text.contains("opaque-ciphertext"));
    assert!(!product_text.contains("provider_state"));
    first.runtime("shutdown_runtime", json!({}));
    drop(first);
    assert_private_state_is_absent(first_host.wait());

    let second_host = HostProcess::start(runtime_home.path());
    let mut second = second_host.connect();
    second.runtime("get_session", json!({"session_id": session_id}));
    let submitted = second.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "RESPONSES_OPAQUE_REPLAY_CASE",
            "variant": "build",
            "idempotency_key": "responses-opaque-replay"
        }),
    );
    let run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("replay run id")
        .to_owned();
    let run = second.wait_for_status(
        "Responses opaque replay",
        &session_id,
        &run_id,
        &["completed"],
    );
    assert_eq!(run["text"], "responses opaque replayed");

    second.runtime("shutdown_runtime", json!({}));
    drop(second);
    assert_private_state_is_absent(second_host.wait());
}

fn assert_private_state_is_absent(output: std::process::Output) {
    assert!(output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!logs.contains("opaque-ciphertext"));
    assert!(!logs.contains("data:image/"));
}
