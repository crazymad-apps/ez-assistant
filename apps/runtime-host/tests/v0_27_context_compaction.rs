#![cfg(unix)]

mod support;

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use serde_json::{Value, json};

use support::{
    Client, FakeProvider, HostProcess, compatibility_headers, model_parameters, provider_fixture,
    test_directory, write_config, write_model_fixture,
};

const TEST_API_KEY: &str = "offline-compaction-secret-must-not-leak";
const SUMMARY_INSTRUCTION: &str =
    "Summarize the earlier conversation for use as context in future turns.";

#[derive(Clone, Default)]
struct ProviderState {
    requests: Arc<Mutex<Vec<Value>>>,
    summary_started: Option<mpsc::Sender<()>>,
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

    let baseline = client.submit_started_input(json!({
        "session_id":session_id,
        "message":"COMPACTION_BASE_CASE establish the compressible head",
        "variant":"build",
        "idempotency_key":"compaction-baseline"
    }));
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

    let submitted = client.submit_started_input(json!({
        "session_id":session_id,
        "message":"COMPACTION_RUN_CASE load the review skill and continue",
        "variant":"build",
        "idempotency_key":"compaction-run"
    }));
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
    assert_eq!(
        view["usage"]["accumulated"],
        json!({
            "input_tokens":20100,"output_tokens":500,"total_tokens":20600,"cached_input_tokens":16000
        })
    );
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
    assert!(summary_requests[0].get("tools").is_none());
    assert!(summary_requests[0].get("tool_choice").is_none());
    assert!(!requests[1]["tools"].as_array().unwrap().is_empty());
    assert_eq!(requests[3]["tools"], requests[1]["tools"]);
    drop(requests);

    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(first_host.wait().status.success());
    assert_local_usage(runtime_home.path(), &session_id);

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    let recovered = session_view(&mut client, &session_id);
    assert_eq!(recovered["conversation"]["generation"], 2);
    assert_eq!(
        recovered["usage"]["accumulated"],
        view["usage"]["accumulated"]
    );
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
    assert_local_usage(runtime_home.path(), &session_id);
    assert_eq!(
        provider_state.requests.lock().unwrap().len(),
        4,
        "reading and restart must not replay a model request"
    );
}

fn assert_local_usage(home: &Path, session: &str) {
    let connection = rusqlite::Connection::open_with_flags(
        home.join("users/_personal/data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let requests:(i64,i64,i64,i64,i64)=connection.query_row(
        "SELECT COUNT(*),SUM(input_tokens),SUM(output_tokens),SUM(total_tokens),SUM(cached_input_tokens) FROM model_request_records WHERE session_id=?1",
        [session], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    assert_eq!(requests, (4, 20100, 500, 20600, 16000));
    assert!(
        connection
            .prepare("SELECT run_id FROM model_request_records")
            .is_err()
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE session_id=?1",
                [session],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn manual_compaction_both_protocols_is_idempotent_after_restart_without_a_run() {
    for protocol in ["chat_completions", "responses"] {
        let state = ProviderState::default();
        let provider = test_provider(&state);
        let home = test_directory();
        write_compaction_config(home.path(), provider.endpoint(), protocol);
        let host = HostProcess::start(home.path());
        let mut client = host.connect();
        let session = create_session(&mut client);
        let noop = compact(&mut client, &session, "empty", 1);
        assert_eq!(noop["outcome"]["type"], "no_op");
        assert!(state.requests.lock().unwrap().is_empty());
        for index in 0..3 {
            complete_turn(&mut client, &session, &format!("MANUAL_BASE_{index}"));
        }
        let before = client.conversation(&session);
        let runs = client.runtime("list_runs", json!({"session_id":session}))["runs"].clone();
        let result = compact(&mut client, &session, "manual-once", 1);
        assert_eq!(result["outcome"]["type"], "compacted");
        assert_eq!(result["outcome"]["source_generation"], 1);
        assert_eq!(result["outcome"]["result_generation"], 2);
        assert_eq!(
            compact(&mut client, &session, "manual-once", 1)["outcome"],
            result["outcome"]
        );
        let after = client.conversation(&session);
        assert_eq!(after["generation"], 2);
        assert_eq!(after["owner"], before["owner"]);
        for item in before["items"].as_array().unwrap() {
            assert!(
                after["items"].as_array().unwrap().contains(item),
                "product history must survive compaction"
            );
        }
        assert_eq!(
            client.runtime("list_runs", json!({"session_id":session}))["runs"],
            runs
        );
        assert_eq!(
            session_view(&mut client, &session)["session"]["queued_input_count"],
            0
        );
        let usage = session_view(&mut client, &session)["usage"].clone();
        assert_summary_wire(&state, 1);
        assert_eq!(state.requests.lock().unwrap().len(), 4);
        stop_host(host, client);
        assert_file_and_controls(home.path(), &session, 2, 4);

        let host = HostProcess::start(home.path());
        let mut client = host.connect();
        assert_eq!(client.conversation(&session), after);
        assert_eq!(session_view(&mut client, &session)["usage"], usage);
        assert_eq!(
            compact(&mut client, &session, "manual-once", 1)["outcome"],
            result["outcome"]
        );
        assert_eq!(
            state.requests.lock().unwrap().len(),
            4,
            "restart and operation reread do not call the model"
        );
        complete_turn(&mut client, &session, "AFTER_MANUAL_RESTART");
        let requests = state.requests.lock().unwrap();
        assert!(
            requests
                .last()
                .unwrap()
                .to_string()
                .contains("baseline summarized")
        );
        assert!(
            !requests.last().unwrap()["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        drop(requests);
        stop_host(host, client);
        assert_file_and_controls(home.path(), &session, 2, 5);
    }
}

#[test]
fn manual_http_errors_and_transient_retry_preserve_the_original_contract() {
    for protocol in ["chat_completions", "responses"] {
        for case in ["M2_BAD_REQUEST", "M2_TRANSIENT"] {
            let state = ProviderState::default();
            let provider = test_provider(&state);
            let home = test_directory();
            write_compaction_config(home.path(), provider.endpoint(), protocol);
            let host = HostProcess::start(home.path());
            let mut client = host.connect();
            let session = create_session(&mut client);
            for index in 0..3 {
                complete_turn(&mut client, &session, &format!("{case}_{index}"));
            }
            let before = client.conversation(&session);
            let (status, result) = raw_compact(&host, &session, "manual-http", 1);
            if case == "M2_BAD_REQUEST" {
                assert!(!status.is_success());
                assert_eq!(result["error"]["code"], "context_compaction_failed");
                assert!(result["error"]["message"].as_str().unwrap().contains("400"));
                assert!(!result.to_string().contains(TEST_API_KEY));
                assert!(!result.to_string().contains("private-upstream-body"));
                assert_eq!(client.conversation(&session), before);
                assert_summary_wire(&state, 1);
            } else {
                assert!(status.is_success(), "{result}");
                assert_eq!(
                    result["result"]["payload"]["payload"]["outcome"]["type"],
                    "compacted"
                );
                assert_summary_wire(&state, 2);
                assert_eq!(client.conversation(&session)["generation"], 2);
            }
            stop_host(host, client);
            assert_file_and_controls(
                home.path(),
                &session,
                if case == "M2_BAD_REQUEST" { 1 } else { 2 },
                if case == "M2_BAD_REQUEST" { 3 } else { 4 },
            );
        }
    }
}

#[test]
fn manual_cancel_both_protocols_keeps_history_and_does_not_create_a_run() {
    for protocol in ["chat_completions", "responses"] {
        let (started, received) = mpsc::channel();
        let state = ProviderState {
            summary_started: Some(started),
            ..Default::default()
        };
        let provider = test_provider(&state);
        let home = test_directory();
        write_compaction_config(home.path(), provider.endpoint(), protocol);
        let host = HostProcess::start(home.path());
        let mut client = host.connect();
        let session = create_session(&mut client);
        for index in 0..3 {
            complete_turn(&mut client, &session, &format!("M2_BLOCK_{index}"));
        }
        let before = client.conversation(&session);
        let runs = client.runtime("list_runs", json!({"session_id":session}))["runs"].clone();
        let mut pending_client = host.connect();
        let pending_session = session.clone();
        let pending =
            thread::spawn(move || compact(&mut pending_client, &pending_session, "cancel-once", 1));
        received
            .recv_timeout(Duration::from_secs(10))
            .expect("summary request reached provider");
        client.runtime(
            "cancel_session_compaction",
            json!({"session_id":session,"operation_id":"cancel-once"}),
        );
        assert_eq!(pending.join().unwrap()["outcome"]["type"], "cancelled");
        assert_eq!(
            compact(&mut client, &session, "cancel-once", 1)["outcome"]["type"],
            "cancelled"
        );
        assert_eq!(client.conversation(&session), before);
        assert_eq!(
            client.runtime("list_runs", json!({"session_id":session}))["runs"],
            runs
        );
        assert_summary_wire(&state, 1);
        stop_host(host, client);
        assert_file_and_controls(home.path(), &session, 1, 3);
    }
}

#[test]
fn child_compaction_owns_its_generation_and_preserves_errors_across_host_restart() {
    for case in ["success", "M2_BAD_REQUEST"] {
        let state = ProviderState::default();
        let provider = test_provider(&state);
        let home = test_directory();
        let workspace = test_directory();
        write_compaction_config(home.path(), provider.endpoint(), "chat_completions");
        let host = HostProcess::start(home.path());
        let mut client = host.connect();
        let session = create_skill_session(&mut client, workspace.path());
        client.runtime(
            "set_session_approval_mode",
            json!({"session_id":session,"approval_mode":"auto"}),
        );
        let parent = complete_turn(&mut client, &session, &format!("M2_PARENT_DELEGATE {case}"));
        let tasks = client.runtime(
            "list_child_tasks",
            json!({"session_id":session,"parent_run_id":parent["run_id"]}),
        )["tasks"]
            .clone();
        assert_eq!(tasks.as_array().unwrap().len(), 1);
        let child = &tasks[0];
        assert_eq!(
            child["status"],
            if case == "success" {
                "completed"
            } else {
                "failed"
            }
        );
        if case != "success" {
            assert_eq!(child["error"]["code"], "context_compaction_failed");
            assert!(child["error"]["message"].as_str().unwrap().contains("400"));
            assert!(!child.to_string().contains("private-upstream-body"));
        }
        let child_view = client.runtime(
            "get_child_task_view",
            json!({"session_id":session,"child_task_id":child["child_task_id"]}),
        )["snapshot"]["value"]
            .clone();
        let child_generation = if case == "success" { 2 } else { 1 };
        assert_eq!(child_view["conversation"]["generation"], child_generation);
        assert_eq!(client.conversation(&session)["generation"], 1);
        assert_summary_wire(&state, 1);
        let request_count = state.requests.lock().unwrap().len();
        assert_eq!(request_count, if case == "success" { 5 } else { 4 });
        stop_host(host, client);
        assert_file_and_controls(
            home.path(),
            &session,
            1,
            if case == "success" { 5 } else { 3 },
        );
        let child_file = home
            .path()
            .join("users/_personal/data/sessions")
            .join(&session)
            .join("child-tasks")
            .join(child["child_task_id"].as_str().unwrap())
            .join(format!("body-{child_generation}.jsonl"));
        let child_frames = read_frames(&child_file);
        assert_eq!(child_frames[0]["generation"], child_generation);
        assert_eq!(
            child_frames[0]["owner"],
            json!({"type":"child_task","session_id":session,"child_task_id":child["child_task_id"]})
        );
        assert!(child_frames.iter().any(|frame| {
            frame["history"]["child_tasks"]
                .as_array()
                .is_some_and(|tasks| !tasks.is_empty())
        }));

        let host = HostProcess::start(home.path());
        let mut client = host.connect();
        assert_eq!(
            client.runtime(
                "get_child_task_view",
                json!({"session_id":session,"child_task_id":child["child_task_id"]})
            )["snapshot"]["value"],
            child_view
        );
        assert_eq!(
            client.runtime(
                "list_child_tasks",
                json!({"session_id":session,"parent_run_id":parent["run_id"]})
            )["tasks"],
            tasks
        );
        assert_eq!(state.requests.lock().unwrap().len(), request_count);
        stop_host(host, client);
    }
}

#[test]
fn automatic_parent_error_retains_http_status_in_file_history() {
    let state = ProviderState::default();
    let provider = test_provider(&state);
    let home = test_directory();
    let workspace = test_directory();
    write_compaction_config(home.path(), provider.endpoint(), "chat_completions");
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let session = create_skill_session(&mut client, workspace.path());
    complete_turn(&mut client, &session, "M2_BAD_REQUEST baseline");
    let submitted=client.submit_started_input(json!({"session_id":session,"message":"COMPACTION_RUN_CASE","variant":"build","idempotency_key":"parent-failure"}));
    let run_id = string(&submitted["run"]["run_id"]);
    let failed = client.wait_for_status("parent summary 400", &session, &run_id, &["failed"]);
    assert_eq!(failed["error"]["code"], "context_compaction_failed");
    assert!(failed["error"]["message"].as_str().unwrap().contains("400"));
    assert!(!failed.to_string().contains(TEST_API_KEY));
    assert_summary_wire(&state, 1);
    stop_host(host, client);
    assert_file_and_controls(home.path(), &session, 1, 2);
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    assert_eq!(
        client.runtime("get_run", json!({"session_id":session,"run_id":run_id}))["run"],
        failed
    );
    assert_eq!(state.requests.lock().unwrap().len(), 3);
    stop_host(host, client);
}

fn create_skill_session(client: &mut Client, workspace: &Path) -> String {
    let directory = workspace.join(".ez-assistant/skills/review");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        "---\nname: review\ndescription: Compaction fixture skill\n---\nKeep the task moving.\n",
    )
    .unwrap();
    let registered = client.runtime(
        "register_workspace",
        json!({"label":"compaction","primary_directory":workspace,"additional_directories":[]}),
    );
    string(&client.runtime("create_session",json!({"title":"F04 automatic","model_selection":null,"workspace_id":registered["workspace"]["workspace_id"]}))["session"]["session_id"])
}

fn read_frames(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn test_provider(state: &ProviderState) -> FakeProvider {
    FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(compaction_provider))
            .route("/v1/responses", post(compaction_provider))
            .with_state(state.clone()),
    )
}

fn write_compaction_config(home: &Path, endpoint: &str, protocol: &str) {
    let mut parameters = model_parameters(false, false, "unsupported");
    parameters["tool_choice"]["none"] = json!("unsupported");
    write_model_fixture(
        home,
        vec![provider_fixture(
            endpoint,
            TEST_API_KEY,
            if protocol == "responses" {
                "openai"
            } else {
                "local"
            },
            protocol,
            vec![json!({"model_id":"offline-model","parameters":parameters,"purpose":"default"})],
        )],
    );
}

fn create_session(client: &mut Client) -> String {
    string(
        &client.runtime(
            "create_session",
            json!({"title":"F04 isolated compaction","model_selection":null}),
        )["session"]["session_id"],
    )
}

fn complete_turn(client: &mut Client, session: &str, message: &str) -> Value {
    let submitted = client.submit_started_input(
        json!({"session_id":session,"message":message,"variant":"build","idempotency_key":message}),
    );
    client.wait_for_status(
        message,
        session,
        submitted["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    )
}

fn compact(client: &mut Client, session: &str, operation: &str, generation: u64) -> Value {
    client.runtime(
        "compact_session",
        json!({"session_id":session,"operation_id":operation,"expected_generation":generation}),
    )
}

fn raw_compact(
    host: &HostProcess,
    session: &str,
    operation: &str,
    generation: u64,
) -> (reqwest::StatusCode, Value) {
    let response = reqwest::blocking::Client::builder().no_proxy().default_headers(compatibility_headers()).timeout(Duration::from_secs(12)).build().unwrap()
        .post(format!("{}/commands", host.base_url())).bearer_auth(host.access_token())
        .json(&json!({"request_id":"manual-http","command":{"scope":"runtime","payload":{"type":"compact_session","payload":{"session_id":session,"operation_id":operation,"expected_generation":generation}}}}))
        .send().unwrap();
    (response.status(), response.json().unwrap())
}

fn stop_host(host: HostProcess, mut client: Client) {
    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(host.wait().status.success());
}

fn assert_summary_wire(state: &ProviderState, count: usize) {
    let requests = state.requests.lock().unwrap();
    let summaries = requests
        .iter()
        .filter(|request| request.to_string().contains(SUMMARY_INSTRUCTION))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), count);
    for summary in summaries {
        assert!(summary.get("tools").is_none());
        assert!(summary.get("tool_choice").is_none());
    }
}

fn assert_file_and_controls(home: &Path, session: &str, generation: i64, requests: i64) {
    let db = rusqlite::Connection::open_with_flags(
        home.join("users/_personal/data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (stored_generation, count): (i64, i64) = db
        .query_row(
            "SELECT body_generation,message_count FROM sessions WHERE session_id=?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_generation, generation);
    let body = home
        .join("users/_personal/data/sessions")
        .join(session)
        .join(format!("conversation.{generation}.jsonl"));
    assert!(body.is_file(), "{}", body.display());
    let lines = fs::read_to_string(&body).unwrap();
    let frames = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        frames[0]["owner"],
        json!({"type":"main_session","session_id":session})
    );
    assert_eq!(frames[0]["generation"], generation);
    // F02 提交帧同时包含控制事实与消息，只累计真实消息的物理数量。
    let messages = frames
        .iter()
        .filter_map(|frame| frame["messages"].as_array())
        .map(Vec::len)
        .sum::<usize>() as i64;
    assert_eq!(messages, count);
    for table in [
        "runs",
        "child_tasks",
        "body_appends",
        "pending_tool_exchanges",
    ] {
        let remaining: i64 = db
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id=?1"),
                [session],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            remaining, 0,
            "{table} retains no completed execution history"
        );
    }
    let recorded: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM model_request_records WHERE session_id=?1",
            [session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, requests);
    let queued:i64 = db.query_row("SELECT COUNT(*) FROM inputs WHERE session_id=?1 AND (state!='committed' OR queued_message_json IS NOT NULL)", [session], |row|row.get(0)).unwrap();
    assert_eq!(queued, 0, "only consumed input identities may remain");
    for table in ["run_message_refs", "message_feedback"] {
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
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
    let request_number = state.requests.lock().unwrap().len();
    if body_text.contains(SUMMARY_INSTRUCTION) {
        if let Some(started) = &state.summary_started {
            let _ = started.send(());
        }
        if body_text.contains("M2_BLOCK") {
            std::future::pending::<()>().await;
        }
        let summaries = state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.to_string().contains(SUMMARY_INSTRUCTION))
            .count();
        if body_text.contains("M2_BAD_REQUEST")
            || body_text.contains("M2_TRANSIENT") && summaries == 1
        {
            let status = if body_text.contains("M2_BAD_REQUEST") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            return (status, Json(json!({"error":{"message":format!("private-upstream-body {TEST_API_KEY}"),"type":"fixture_error"}}))).into_response();
        }
    }
    let response = if body_text.contains(SUMMARY_INSTRUCTION) {
        text_response(
            "compaction-summary",
            "baseline summarized",
            6_900,
            100,
            6_000,
        )
    } else if body_text.contains("M2_PARENT_DELEGATE") {
        if body_text.contains("call-delegate-compaction") {
            text_response("compaction-final", "parent completed", 100, 20, 0)
        } else {
            named_tool_response(
                "delegate_task",
                "call-delegate-compaction",
                json!({"title":"Compacting child","task":format!("M2_CHILD_WORK {} {}", if body_text.contains("M2_BAD_REQUEST") {"M2_BAD_REQUEST"} else {"success"}, "Preserve all prior task details while continuing. ".repeat(80))}),
                100,
            )
        }
    } else if body_text.contains("M2_CHILD_WORK")
        && !body_text.contains("baseline summarized")
        && !body_text.contains("call-load-review-compaction")
    {
        tool_response()
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
    let response = response
        .replace(
            "compaction-baseline",
            &format!("compaction-baseline-{request_number}"),
        )
        .replace(
            "compaction-final",
            &format!("compaction-final-{request_number}"),
        )
        .replace(
            "compaction-summary",
            &format!("compaction-summary-{request_number}"),
        );
    let response = if body.get("input").is_some() {
        chat_stream_as_responses(&response)
    } else {
        response
    };
    ([("content-type", "text/event-stream")], response).into_response()
}

fn chat_stream_as_responses(chat: &str) -> String {
    let chunks = chat
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let first = &chunks[0];
    let id = first["id"].as_str().unwrap();
    let text = first["choices"][0]["delta"]["content"].as_str().unwrap();
    let usage = &chunks.last().unwrap()["usage"];
    [
        json!({"type":"response.created","response":{"id":id,"model":"offline-model","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":format!("msg-{id}"),"type":"message","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.done","item_id":format!("msg-{id}"),"output_index":0,"content_index":0,"text":text}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"id":format!("msg-{id}"),"type":"message","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]}}),
        json!({"type":"response.completed","response":{"id":id,"model":"offline-model","status":"completed","usage":{"input_tokens":usage["prompt_tokens"],"output_tokens":usage["completion_tokens"],"total_tokens":usage["total_tokens"],"input_tokens_details":{"cached_tokens":usage["prompt_tokens_details"]["cached_tokens"]}}}}),
    ].into_iter().map(|event|format!("data: {event}\n\n")).collect::<String>()+"data: [DONE]\n\n"
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
    named_tool_response(
        "load_skill",
        "call-load-review-compaction",
        json!({"name":"review"}),
        7_400,
    )
}

fn named_tool_response(name: &str, call_id: &str, arguments: Value, input_tokens: u64) -> String {
    let arguments = serde_json::to_string(&arguments).unwrap();
    let first = json!({
        "id":"compaction-tool",
        "model":"offline-model",
        "choices":[{
            "index":0,
            "delta":{
                "role":"assistant",
                "tool_calls":[{
                    "index":0,
                    "id":call_id,
                    "type":"function",
                    "function":{"name":name,"arguments":arguments}
                }]
            },
            "finish_reason":null
        }]
    });
    // 正式 Runtime 在窗口占用达到 90% 时压缩；夹具需越过 8192 * 0.9。
    let terminal = json!({
        "id":"compaction-tool",
        "model":"offline-model",
        "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],
        "usage":{
            "prompt_tokens":input_tokens,
            "completion_tokens":200,
            "total_tokens":input_tokens+200,
            "prompt_tokens_details":{"cached_tokens":input_tokens.min(5_500)}
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
