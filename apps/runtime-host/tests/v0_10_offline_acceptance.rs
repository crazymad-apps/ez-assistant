#![cfg(unix)]

mod support;

use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

const TEST_API_KEY: &str = "offline-secret-must-not-leak";

#[test]
fn two_real_host_processes_recover_and_complete_the_v0_10_session_lifecycle() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);

    let first_host = HostProcess::start(runtime_home.path());
    let mut first_client = first_host.connect();
    let configuration = first_client.runtime("get_config_status", json!({}));
    assert_eq!(
        configuration["status"]["state"], "ready",
        "offline test configuration was not ready: {configuration}"
    );
    let created = first_client.runtime(
        "create_session",
        json!({ "title": "Offline acceptance", "model_key": "fixture" }),
    );
    let session_id = string(&created["session"]["session_id"]);

    let first = submit(&mut first_client, &session_id, "FIRST_CASE", "first-submit");
    let first_run = string(&first["run"]["run_id"]);
    let duplicate = submit(&mut first_client, &session_id, "FIRST_CASE", "first-submit");
    assert_eq!(duplicate["input_id"], first["input_id"]);
    assert_eq!(duplicate["run"]["run_id"], first["run"]["run_id"]);
    assert_eq!(
        first_client.wait_for_status("first run", &session_id, &first_run, &["completed"])["status"],
        "completed"
    );
    let blocked = submit(
        &mut first_client,
        &session_id,
        "BLOCK_FOR_RESTART",
        "blocked-submit",
    );
    let blocked_run = string(&blocked["run"]["run_id"]);
    first_client.wait_for_status("restart blocker", &session_id, &blocked_run, &["running"]);
    let queued = submit(
        &mut first_client,
        &session_id,
        "QUEUED_AFTER_RESTART",
        "queued-submit",
    );
    let queued_run = string(&queued["run"]["run_id"]);
    assert_eq!(queued["run"]["status"], "accepted");

    // 断开并重连客户端不会取消 Runtime-owned Run；查询只依赖权威快照。
    drop(first_client);
    let mut reconnected = first_host.connect();
    assert_eq!(
        reconnected.wait_for_status(
            "reconnected blocker",
            &session_id,
            &blocked_run,
            &["running"],
        )["status"],
        "running"
    );
    let first_output = first_host.kill();
    assert_output_is_safe(&first_output);

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    let sessions = client.runtime("list_sessions", json!({}));
    assert_eq!(sessions["sessions"].as_array().expect("sessions").len(), 1);
    let summary = &sessions["sessions"][0];
    assert_eq!(summary["session_id"], session_id);
    assert_eq!(summary["resume_required"], true);
    assert_eq!(summary["queued_input_count"], 1);

    let runs = client.runtime("list_runs", json!({ "session_id": session_id }));
    assert_run_status(&runs, &first_run, "completed");
    assert_run_status(&runs, &blocked_run, "interrupted");
    assert_run_status(&runs, &queued_run, "accepted");
    let recovered_conversation = client.conversation(&session_id);
    assert!(serialized(&recovered_conversation).contains("FIRST_CASE"));
    assert!(serialized(&recovered_conversation).contains("BLOCK_FOR_RESTART"));
    assert!(!serialized(&recovered_conversation).contains("QUEUED_AFTER_RESTART"));

    client.runtime("resume_session", json!({ "session_id": session_id }));
    assert_eq!(
        client.wait_for_status("resumed queue", &session_id, &queued_run, &["completed"])["status"],
        "completed"
    );

    let tool = submit(&mut client, &session_id, "TOOL_CASE", "tool-submit");
    let tool_run = string(&tool["run"]["run_id"]);
    let pending = wait_for_pending_approval(&mut client, &session_id);
    assert_eq!(pending["subject"]["type"], "file");
    assert_eq!(pending["subject"]["tool_name"], "list_directory");
    assert_eq!(pending["subject"]["operation"], "list");
    assert_eq!(
        pending["available_decisions"],
        json!(["allow_once", "allow_session", "deny"])
    );
    client.runtime(
        "decide_approval",
        json!({
            "session_id": session_id,
            "approval_id": pending["approval_id"],
            "decision": "allow_session"
        }),
    );
    assert_eq!(
        client.wait_for_status("tool run", &session_id, &tool_run, &["completed"])["status"],
        "completed"
    );
    let tool_conversation = client.conversation(&session_id);
    let tool_json = serialized(&tool_conversation);
    assert!(tool_json.contains("call-list-directory-1"));
    assert!(tool_json.contains("list_directory"));

    let repeated_tool = submit(
        &mut client,
        &session_id,
        "TOOL_CASE",
        "tool-submit-after-session-allow",
    );
    let repeated_tool_run = string(&repeated_tool["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "tool run covered by session rule",
            &session_id,
            &repeated_tool_run,
            &["completed"],
        )["status"],
        "completed"
    );
    assert!(
        client.runtime(
            "list_pending_approvals",
            json!({ "session_id": session_id })
        )["approvals"]
            .as_array()
            .expect("approval list")
            .is_empty()
    );

    let cancelling = submit(&mut client, &session_id, "CANCEL_CASE", "cancel-submit");
    let cancelling_run = string(&cancelling["run"]["run_id"]);
    client.wait_for_status(
        "cancellable run",
        &session_id,
        &cancelling_run,
        &["running"],
    );
    client.runtime(
        "cancel_run",
        json!({ "session_id": session_id, "run_id": cancelling_run }),
    );
    assert_eq!(
        client.wait_for_status(
            "cancelled run",
            &session_id,
            &cancelling_run,
            &["cancelled"],
        )["status"],
        "cancelled"
    );

    let changed = client.runtime(
        "set_session_model",
        json!({ "session_id": session_id, "model_key": "alternate" }),
    );
    assert_eq!(changed["session"]["model_key"], "alternate");
    let before_reentry = client.conversation(&session_id);
    let target_message_id = first_user_message_id(&before_reentry);
    let replacement = client.runtime(
        "reenter_from_user_message",
        json!({
            "session_id": session_id,
            "message_id": target_message_id,
            "message": "REPLACEMENT_CASE",
            "variant": "build",
            "idempotency_key": "replacement-submit"
        }),
    );
    let replacement_run = string(&replacement["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "replacement run",
            &session_id,
            &replacement_run,
            &["completed"],
        )["status"],
        "completed"
    );
    let replacement_conversation = client.conversation(&session_id);
    let replacement_json = serialized(&replacement_conversation);
    assert!(replacement_json.contains("REPLACEMENT_CASE"));
    assert!(replacement_json.contains("replacement answer"));
    assert!(!replacement_json.contains("FIRST_CASE"));
    assert_eq!(
        client.runtime("list_runs", json!({ "session_id": session_id }))["runs"]
            .as_array()
            .expect("replacement runs")
            .len(),
        1
    );

    let archived = client.runtime("archive_session", json!({ "session_id": session_id }));
    assert_eq!(archived["session"]["lifecycle"], "archived");
    assert!(
        client.runtime("list_sessions", json!({}))["sessions"]
            .as_array()
            .expect("active sessions")
            .is_empty()
    );
    assert_eq!(
        client.runtime("list_sessions", json!({ "filter": "archived" }))["sessions"]
            .as_array()
            .expect("archived sessions")
            .len(),
        1
    );
    assert_eq!(client.conversation(&session_id), replacement_conversation);
    let restored = client.runtime("restore_session", json!({ "session_id": session_id }));
    assert_eq!(restored["session"]["lifecycle"], "active");

    let stopped = client.runtime("shutdown_runtime", json!({}));
    assert_eq!(stopped["lifecycle"], "stopped");
    drop(client);
    let second_output = second_host.wait();
    assert!(second_output.status.success());
    assert_output_is_safe(&second_output);

    verify_physical_state(runtime_home.path(), &session_id);
}

fn submit(client: &mut Client, session_id: &str, message: &str, key: &str) -> Value {
    client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": message,
            "variant": "build",
            "idempotency_key": key
        }),
    )
}

fn wait_for_pending_approval(client: &mut Client, session_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let mut approvals = client.runtime(
            "list_pending_approvals",
            json!({ "session_id": session_id }),
        )["approvals"]
            .as_array()
            .expect("approval list")
            .clone();
        if let Some(approval) = approvals.pop() {
            return approval;
        }
        assert!(Instant::now() < deadline, "approval did not become pending");
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_run_status(result: &Value, run_id: &str, expected: &str) {
    let run = result["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .find(|run| run["run_id"] == run_id)
        .expect("run in list");
    assert_eq!(run["status"], expected);
}

fn first_user_message_id(conversation: &Value) -> String {
    conversation["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["turn"]["id"].as_str())
        .expect("first User Message ID")
        .to_owned()
}

fn verify_physical_state(runtime_home: &Path, session_id: &str) {
    let database = runtime_home.join("data/runtime.sqlite3");
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open acceptance database read-only phase");
    let (lifecycle, model_key, generation, message_count): (String, String, i64, i64) = connection
        .query_row(
            "SELECT lifecycle, model_key, body_generation, message_count
             FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("session row");
    assert_eq!(lifecycle, "active");
    assert_eq!(model_key, "alternate");
    assert!(generation > 1);
    assert_eq!(message_count, 2);
    for (table, expected) in [("inputs", 1_i64), ("runs", 1), ("run_message_refs", 2)] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("table count");
        assert_eq!(count, expected, "unexpected {table} count");
    }
    for table in ["pending_tool_exchanges", "body_appends"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("temporary table count");
        assert_eq!(count, 0, "temporary {table} was not cleared");
    }

    let body = runtime_home.join(format!(
        "data/sessions/{session_id}/conversation.{generation}.jsonl"
    ));
    let content = fs::read_to_string(body).expect("read authoritative conversation JSONL");
    assert_eq!(content.lines().count(), 2);
    assert!(content.contains("REPLACEMENT_CASE"));
    assert!(content.contains("replacement answer"));
    assert!(!content.contains("FIRST_CASE"));
}

fn assert_output_is_safe(output: &std::process::Output) {
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        TEST_API_KEY,
        "FIRST_CASE",
        "BLOCK_FOR_RESTART",
        "REPLACEMENT_CASE",
    ] {
        assert!(
            !output.contains(secret),
            "Host output leaked a sensitive marker"
        );
    }
}

fn serialized(value: &Value) -> String {
    serde_json::to_string(value).expect("serialize JSON assertion value")
}

fn string(value: &Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
