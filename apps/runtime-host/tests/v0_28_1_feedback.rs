//! 问答正式 Host／文件历史／SQL pending／重启验收；仅使用新建隔离 Home。
#![cfg(unix)]
mod support;
use axum::{Json, Router, routing::post};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
use support::{Client, FakeProvider, HostProcess, write_config};
static CALLS: AtomicU64 = AtomicU64::new(0);
async fn provider(Json(body): Json<Value>) -> impl axum::response::IntoResponse {
    let messages = body["messages"].as_array().unwrap();
    let last_user = messages.iter().rposition(|m| m["role"] == "user").unwrap();
    let content = messages[last_user]["content"].to_string();
    let already = messages[last_user + 1..]
        .iter()
        .any(|m| m["role"] == "tool");
    let number = CALLS.fetch_add(1, Ordering::SeqCst);
    let id = format!("feedback-{number}");
    let (delta, finish) = if !already && content.contains("F04_") {
        let args = json!({"mode":if content.contains("F04_END") {"end_turn"} else {"wait"},"questions":[{"question":"选择交付格式","options":[{"label":"PDF"},{"label":"文档"}]}]});
        (
            json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":"ask_user","arguments":args.to_string()}}]}),
            "tool_calls",
        )
    } else {
        (json!({"role":"assistant","content":"已收到反馈"}), "stop")
    };
    let first = json!({"id":id,"model":"offline-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let last = json!({"id":id,"model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":20,"completion_tokens":5,"total_tokens":25}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
    )
}
fn pending(client: &mut Client, session: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let records = client.runtime("list_feedback_requests", json!({"session_id":session}));
        if let Some(request) = records
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "pending")
        {
            return request.clone();
        }
        assert!(Instant::now() < deadline, "waiting question missing");
        thread::sleep(Duration::from_millis(20));
    }
}
fn submit(client: &mut Client, session: &str, text: &str) -> String {
    client.submit_started_input(json!({"session_id":session,"message":text,"mode":"normal","variant":"build","attachment_ids":[],"quotes":[]}))["run"]["run_id"].as_str().unwrap().into()
}
fn create(client: &mut Client) -> String {
    client.runtime("create_session", json!({"title":"F04 isolated"}))["session"]["session_id"]
        .as_str()
        .unwrap()
        .into()
}
fn answer(client: &mut Client, request: &Value) -> Value {
    client.runtime("answer_feedback",json!({"session_id":request["session_id"],"request_id":request["request_id"],"answers":[{"selected_options":[0],"text":"保留源文件"}]}))
}
fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn file_feedback_answers_are_durable_sql_keeps_only_pending_and_restart_never_replays_tools() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let mut home = tempfile::Builder::new()
        .prefix("f04-host-")
        .tempdir_in(&root)
        .unwrap();
    if std::env::var_os("EZ_ASSISTANT_KEEP_TEST_FIXTURES").is_some() {
        home.disable_cleanup(true);
    }
    let provider =
        FakeProvider::with_router(Router::new().route("/v1/chat/completions", post(provider)));
    write_config(
        home.path(),
        provider.endpoint(),
        "isolated-feedback-fixture",
    );
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let waiting = create(&mut client);
    let wait_run = submit(&mut client, &waiting, "F04_WAIT");
    let wait_request = pending(&mut client, &waiting);
    let ending = create(&mut client);
    let end_run = submit(&mut client, &ending, "F04_END");
    let end_request = pending(&mut client, &ending);
    client.wait_for_status(
        "end_turn completes without model followup",
        &ending,
        &end_run,
        &["completed"],
    );
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    let database = home.path().join("users/_personal/data/runtime.sqlite3");
    let before = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(count(&before, "session_feedback"), 2);
    let backup = home.path().join("before-restart.sqlite3");
    before.backup("main", &backup, None).unwrap();
    let backed = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in ["sessions", "runs", "inputs", "session_feedback"] {
        assert_eq!(count(&backed, table), count(&before, table));
    }
    drop(backed);
    drop(before);
    host.kill();
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let restored = client.runtime("list_feedback_requests", json!({"session_id":ending}));
    assert_eq!(restored[0]["status"], "pending");
    let invalid = client.runtime("list_feedback_requests", json!({"session_id":waiting}));
    assert_eq!(invalid[0]["status"], "invalidated");
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    let accepted = answer(&mut client, &end_request);
    assert_eq!(accepted["status"], "answered");
    assert_eq!(answer(&mut client, &end_request), accepted);
    let deadline = Instant::now() + Duration::from_secs(12);
    let runs = loop {
        let runs = client.runtime("list_runs", json!({"session_id":ending}));
        if runs["runs"].as_array().unwrap().len() == 2 {
            break runs;
        }
        assert!(
            Instant::now() < deadline,
            "feedback reply was not claimed: {runs}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let new_run = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["run_id"] != end_run)
        .unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.wait_for_status("feedback reply", &ending, &new_run, &["completed"]);
    let response=reqwest::blocking::Client::new().post(format!("{}/commands",host.base_url())).headers(support::compatibility_headers()).bearer_auth(host.access_token()).json(&json!({"request_id":"late-reply","command":{"scope":"runtime","payload":{"type":"answer_feedback","payload":{"session_id":waiting,"request_id":wait_request["request_id"],"answers":[{"selected_options":[0],"text":"late"}]}}}})).send().unwrap();
    assert!(!response.status().is_success());
    assert_eq!(
        response.json::<Value>().unwrap()["error"]["code"],
        "invalid_request"
    );
    let invalid_run = client.runtime("get_run", json!({"session_id":waiting,"run_id":wait_run}));
    assert_eq!(invalid_run["run"]["status"], "interrupted");
    assert_eq!(CALLS.load(Ordering::SeqCst), 3);
    let answered = client.runtime("list_feedback_requests", json!({"session_id":ending}));
    assert_eq!(answered[0], accepted);
    assert_eq!(
        client.runtime("list_feedback_requests", json!({"session_id":waiting})),
        invalid
    );
    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(count(&db, "session_feedback"), 0);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    eprintln!(
        "Feedback verified: database={}, backup={}, SQL pending=0, file answered=1/invalidated=1, sessions={}, runs={}, inputs={}",
        database.display(),
        backup.display(),
        count(&db, "sessions"),
        count(&db, "runs"),
        count(&db, "inputs")
    );
    drop(db);
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    assert_eq!(
        client.runtime("list_feedback_requests", json!({"session_id":ending})),
        answered
    );
    assert_eq!(
        client.runtime("list_feedback_requests", json!({"session_id":waiting})),
        invalid
    );
    assert_eq!(answer(&mut client, &end_request), accepted);
    assert_eq!(CALLS.load(Ordering::SeqCst), 3);
    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
}
