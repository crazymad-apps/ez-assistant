#![cfg(unix)]

mod support;

use std::{
    fs,
    sync::{Arc, Mutex},
};

use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use serde_json::{Value, json};

use support::{Client, FakeProvider, HostProcess, test_directory, write_config};

const TEST_API_KEY: &str = "offline-compaction-secret-must-not-leak";
const SUMMARY_INSTRUCTION: &str =
    "Summarize the earlier conversation for use as context in future turns.";

#[derive(Clone, Default)]
struct ProviderState {
    requests: Arc<Mutex<Vec<Value>>>,
}

#[test]
fn formal_host_compacts_once_continues_the_same_run_and_recovers_generation() {
    let provider_state = ProviderState::default();
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(compaction_provider))
            .with_state(provider_state.clone()),
    );
    let runtime_home = test_directory();
    let workspace = test_directory();
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);
    let skill_directory = workspace.path().join(".ez-assistant/skills/review");
    fs::create_dir_all(&skill_directory).expect("create fixture skill directory");
    fs::write(
        skill_directory.join("SKILL.md"),
        "---\nname: review\ndescription: Compaction fixture skill\n---\nKeep the compacted task moving.\n",
    )
    .expect("write fixture skill");

    let first_host = HostProcess::start(runtime_home.path());
    let mut client = first_host.connect();
    let workspace_id = string(
        &client.runtime(
            "register_workspace",
            json!({
                "label":"compaction-workspace",
                "primary_directory":workspace.path(),
                "additional_directories":[]
            }),
        )["workspace"]["workspace_id"],
    );
    let session_id = string(
        &client.runtime(
            "create_session",
            json!({
                "title":"Context compaction acceptance",
                "model_selection":null,
                "workspace_id":workspace_id
            }),
        )["session"]["session_id"],
    );

    let baseline = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":"COMPACTION_BASE_CASE establish the compressible head",
            "variant":"build",
            "idempotency_key":"compaction-baseline"
        }),
    );
    let baseline_run_id = string(&baseline["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "compaction baseline",
            &session_id,
            &baseline_run_id,
            &["completed"]
        )["status"],
        "completed"
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":"COMPACTION_RUN_CASE load the review skill and continue",
            "variant":"build",
            "idempotency_key":"compaction-run"
        }),
    );
    let run_id = string(&submitted["run"]["run_id"]);
    let completed = client.wait_for_status(
        "compaction continuation",
        &session_id,
        &run_id,
        &["completed", "failed"],
    );
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["text"], "formal Host continued after compaction");
    assert_eq!(
        completed["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .filter(|tool| tool["tool_name"] == "load_skill")
            .count(),
        1,
        "the pre-compaction tool must not be replayed"
    );

    let view = session_view(&mut client, &session_id);
    assert_eq!(view["conversation"]["generation"], 2);
    let requests = provider_state.requests.lock().expect("provider requests");
    assert_eq!(
        requests.len(),
        4,
        "baseline, tool, summary, resumed request"
    );
    let summary_requests = requests
        .iter()
        .filter(|request| request.to_string().contains(SUMMARY_INSTRUCTION))
        .collect::<Vec<_>>();
    assert_eq!(summary_requests.len(), 1, "exactly one summary request");
    assert_eq!(summary_requests[0]["tools"], requests[1]["tools"]);
    assert_eq!(summary_requests[0]["tool_choice"], "none");
    drop(requests);

    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    let recovered = session_view(&mut client, &session_id);
    assert_eq!(recovered["conversation"]["generation"], 2);
    assert!(
        recovered["conversation"]["items"]
            .as_array()
            .expect("recovered conversation")
            .iter()
            .any(|item| item["type"] == "context_summary")
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(second_host.wait().status.success());
}

async fn compaction_provider(
    State(state): State<ProviderState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    state
        .requests
        .lock()
        .expect("provider request log")
        .push(body.clone());
    let body_text = body.to_string();
    let response = if body_text.contains(SUMMARY_INSTRUCTION) {
        text_response(
            "compaction-summary",
            "baseline summarized",
            6_900,
            100,
            6_000,
        )
    } else if body_text.contains("baseline summarized") {
        text_response(
            "compaction-final",
            "formal Host continued after compaction",
            900,
            100,
            500,
        )
    } else if body_text.contains("COMPACTION_RUN_CASE")
        && !body_text.contains("call-load-review-compaction")
    {
        tool_response()
    } else if body_text.contains("COMPACTION_RUN_CASE") {
        text_response(
            "compaction-final",
            "formal Host continued after compaction",
            900,
            100,
            500,
        )
    } else {
        text_response(
            "compaction-baseline",
            "baseline established",
            4_900,
            100,
            4_000,
        )
    };
    ([("content-type", "text/event-stream")], response)
}

fn text_response(
    response_id: &str,
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> String {
    let first = json!({
        "id":response_id,
        "model":"offline-model",
        "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]
    });
    let terminal = json!({
        "id":response_id,
        "model":"offline-model",
        "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
        "usage":{
            "prompt_tokens":input_tokens,
            "completion_tokens":output_tokens,
            "total_tokens":input_tokens + output_tokens,
            "prompt_tokens_details":{"cached_tokens":cached_tokens}
        }
    });
    format!("data: {first}\n\ndata: {terminal}\n\ndata: [DONE]\n\n")
}

fn tool_response() -> String {
    let arguments =
        serde_json::to_string(&json!({"name":"review"})).expect("serialize load_skill arguments");
    let first = json!({
        "id":"compaction-tool",
        "model":"offline-model",
        "choices":[{
            "index":0,
            "delta":{
                "role":"assistant",
                "tool_calls":[{
                    "index":0,
                    "id":"call-load-review-compaction",
                    "type":"function",
                    "function":{"name":"load_skill","arguments":arguments}
                }]
            },
            "finish_reason":null
        }]
    });
    let terminal = json!({
        "id":"compaction-tool",
        "model":"offline-model",
        "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
        "usage":{
            "prompt_tokens":6_800,
            "completion_tokens":200,
            "total_tokens":7_000,
            "prompt_tokens_details":{"cached_tokens":5_500}
        }
    });
    format!("data: {first}\n\ndata: {terminal}\n\ndata: [DONE]\n\n")
}

fn session_view(client: &mut Client, session_id: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session_id}))["snapshot"]["value"]
        .clone()
}

fn string(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}
