#![cfg(unix)]

mod support;

use serde_json::json;
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_vllm_config};

const TEST_API_KEY: &str = "offline-vllm-secret-must-not-leak";

#[test]
fn formal_host_projects_vllm_reasoning_into_run_and_conversation() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_vllm_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);

    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({"title":"vLLM reasoning", "model_key":"vllm-fixture"}),
        )["session"]["session_id"],
    );
    let run_id = text(
        &client.runtime(
            "submit_input",
            json!({
                "session_id":session_id,
                "message":"VLLM_REASONING_CASE",
                "variant":"build",
                "idempotency_key":"vllm-reasoning"
            }),
        )["run"]["run_id"],
    );
    let run = client.wait_for_status("vLLM reasoning", &session_id, &run_id, &["completed"]);
    assert_eq!(run["reasoning"], "offline vLLM thought");
    assert_eq!(run["text"], "offline vLLM answer");

    let conversation = client.conversation(&session_id);
    let assistant = conversation["items"]
        .as_array()
        .expect("Conversation items")
        .iter()
        .find(|item| item["type"] == "assistant")
        .expect("Assistant product message");
    let segments = assistant["segments"]
        .as_array()
        .expect("Assistant segments");
    assert!(segments.iter().any(|segment| {
        segment["type"] == "reasoning" && segment["text"] == "offline vLLM thought"
    }));
    assert!(
        segments.iter().any(|segment| {
            segment["type"] == "text" && segment["text"] == "offline vLLM answer"
        })
    );

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    let output = host.wait();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TEST_API_KEY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TEST_API_KEY));
}

fn text(value: &serde_json::Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
