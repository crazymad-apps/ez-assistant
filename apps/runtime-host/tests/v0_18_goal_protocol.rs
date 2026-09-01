#![cfg(unix)]

mod support;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

const TEST_API_KEY: &str = "offline-goal-secret-must-not-leak";

#[test]
fn formal_host_projects_and_controls_goal_and_work_plan_across_restart() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);

    let first_host = HostProcess::start(runtime_home.path());
    let mut client = first_host.connect();

    let long_session = create_session(&mut client, "Goal long chain");
    let first = submit_goal(&mut client, &long_session, "GOAL_LONG_CASE", "goal-long");
    let first_run_id = string(&first["run"]["run_id"]);
    let long_view = wait_for_goal_clear(&mut client, &long_session);
    assert_eq!(long_view["composer_capabilities"]["goal_supported"], true);
    assert!(long_view["goal"].is_null());
    assert_eq!(long_view["work_plan"]["revision"], 1);
    assert_eq!(
        long_view["work_plan"]["objective"],
        "complete the offline Goal lifecycle"
    );
    assert!(
        long_view["queue"]["items"]
            .as_array()
            .expect("queue")
            .is_empty()
    );
    let runs = client.runtime("list_runs", json!({"session_id":long_session}));
    assert_eq!(runs["runs"].as_array().expect("Goal runs").len(), 1);
    assert_eq!(
        client.wait_for_status(
            "first Goal run",
            &long_session,
            &first_run_id,
            &["completed"]
        )["status"],
        "completed"
    );

    let blocked_session = create_session(&mut client, "Goal blocked resume");
    let blocked = submit_goal(
        &mut client,
        &blocked_session,
        "GOAL_BLOCK_CASE",
        "goal-blocked",
    );
    let blocked_run = string(&blocked["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "blocked Goal run",
            &blocked_session,
            &blocked_run,
            &["completed"],
        )["status"],
        "completed"
    );
    let blocked_view = wait_for_goal_state(&mut client, &blocked_session, "paused");
    assert_eq!(blocked_view["goal"]["pause_reason"]["type"], "blocked");
    assert_eq!(
        blocked_view["goal"]["pause_reason"]["summary"],
        "need explicit user confirmation"
    );
    let resumed = client.runtime(
        "resume_goal",
        json!({
            "session_id":blocked_session,
            "goal_id":blocked_view["goal"]["goal_id"],
            "expected_generation":blocked_view["goal"]["generation"]
        }),
    );
    let resumed_run = string(&resumed["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "resumed blocked Goal",
            &blocked_session,
            &resumed_run,
            &["completed"],
        )["status"],
        "completed"
    );
    wait_for_goal_clear(&mut client, &blocked_session);

    let stopped_session = create_session(&mut client, "Goal stop");
    let stopping = submit_goal(&mut client, &stopped_session, "GOAL_STOP_CASE", "goal-stop");
    let stopping_run = string(&stopping["run"]["run_id"]);
    client.wait_for_status(
        "Goal stop target",
        &stopped_session,
        &stopping_run,
        &["running"],
    );
    let stopping_view = session_view(&mut client, &stopped_session);
    let stopped = client.runtime(
        "stop_goal",
        json!({
            "session_id":stopped_session,
            "goal_id":stopping_view["goal"]["goal_id"],
            "expected_generation":stopping_view["goal"]["generation"]
        }),
    );
    assert_eq!(stopped["goal"]["state"], "paused");
    assert_eq!(stopped["goal"]["pause_reason"]["type"], "user_stopped");
    assert_eq!(
        client.wait_for_status(
            "stopped Goal Run",
            &stopped_session,
            &stopping_run,
            &["cancelled"],
        )["status"],
        "cancelled"
    );

    let recovery_session = create_session(&mut client, "Goal recovery");
    let recovery = submit_goal(
        &mut client,
        &recovery_session,
        "GOAL_RECOVERY_CASE",
        "goal-recovery",
    );
    let recovery_run = string(&recovery["run"]["run_id"]);
    client.wait_for_status(
        "Goal recovery target",
        &recovery_session,
        &recovery_run,
        &["running"],
    );
    drop(client);
    let first_output = first_host.kill();
    assert!(!String::from_utf8_lossy(&first_output.stdout).contains(TEST_API_KEY));
    assert!(!String::from_utf8_lossy(&first_output.stderr).contains(TEST_API_KEY));

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    let recovery_view = wait_for_goal_state(&mut client, &recovery_session, "paused");
    assert_eq!(
        recovery_view["goal"]["pause_reason"]["type"],
        "recovery_required"
    );
    let resumed = client.runtime(
        "resume_goal",
        json!({
            "session_id":recovery_session,
            "goal_id":recovery_view["goal"]["goal_id"],
            "expected_generation":recovery_view["goal"]["generation"]
        }),
    );
    let resumed_run = string(&resumed["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(
            "recovered Goal",
            &recovery_session,
            &resumed_run,
            &["completed"],
        )["status"],
        "completed"
    );
    wait_for_goal_clear(&mut client, &recovery_session);

    assert_eq!(
        wait_for_goal_state(&mut client, &stopped_session, "paused")["goal"]["pause_reason"]["type"],
        "user_stopped"
    );
    assert_eq!(
        wait_for_goal_clear(&mut client, &long_session)["work_plan"]["revision"],
        1
    );
    let product_conversation = client.conversation(&long_session);
    assert_eq!(
        product_conversation["items"]
            .as_array()
            .expect("product Conversation")
            .iter()
            .filter(|item| item["type"] == "user")
            .count(),
        1,
        "Runtime continuation must stay out of the product transcript"
    );

    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    let output = second_host.wait();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TEST_API_KEY));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TEST_API_KEY));
    verify_long_goal_physical_state(runtime_home.path(), &long_session);
}

fn verify_long_goal_physical_state(runtime_home: &std::path::Path, session_id: &str) {
    let database = runtime_home.join("data/runtime.sqlite3");
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open Goal acceptance database read-only");
    let (generation, message_count): (i64, i64) = connection
        .query_row(
            "SELECT body_generation, message_count FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("Goal Session row");
    let goal_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM session_goals WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .expect("Goal row count");
    assert_eq!(goal_count, 0, "completed Goal control row must be cleared");
    let work_plan_revision: i64 = connection
        .query_row(
            "SELECT revision FROM session_work_plans WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .expect("persisted WorkPlan revision");
    assert_eq!(work_plan_revision, 1);

    let input_counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN origin = 'user' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN origin = 'runtime' THEN 1 ELSE 0 END)
             FROM inputs WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("Goal Input counts");
    assert_eq!(input_counts, (1, 1, 0));
    let completed_runs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE session_id = ?1 AND status = 'completed'",
            [session_id],
            |row| row.get(0),
        )
        .expect("completed Goal Run count");
    assert_eq!(completed_runs, 1);
    drop(connection);

    let body = runtime_home.join(format!(
        "data/sessions/{session_id}/conversation.{generation}.jsonl"
    ));
    let content = fs::read_to_string(body).expect("read authoritative Goal Conversation JSONL");
    assert_eq!(
        i64::try_from(content.lines().count()).expect("JSONL line count fits i64"),
        message_count
    );
    let messages = content
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid Goal JSONL record"))
        .collect::<Vec<_>>();
    let visible_users = messages
        .iter()
        .filter(|message| {
            message["role"] == "user"
                && message["turn"]["origin"] == "user"
                && message["turn"]["transcript_visibility"] == "visible"
        })
        .count();
    let hidden_runtime_users = messages
        .iter()
        .filter(|message| {
            message["role"] == "user"
                && message["turn"]["origin"] == "runtime"
                && message["turn"]["transcript_visibility"] == "hidden"
        })
        .count();
    assert_eq!(visible_users, 1);
    assert_eq!(hidden_runtime_users, 3);
}

fn create_session(client: &mut Client, title: &str) -> String {
    string(
        &client.runtime(
            "create_session",
            json!({"title":title,"model_key":"fixture"}),
        )["session"]["session_id"],
    )
}

fn submit_goal(client: &mut Client, session_id: &str, message: &str, key: &str) -> Value {
    client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":message,
            "variant":"build",
            "mode":"start_goal",
            "idempotency_key":key
        }),
    )
}

fn session_view(client: &mut Client, session_id: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session_id}))["snapshot"]["value"]
        .clone()
}

fn wait_for_goal_state(client: &mut Client, session_id: &str, expected: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let view = session_view(client, session_id);
        if view["goal"]["state"] == expected {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Goal did not reach {expected}: {view}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_goal_clear(client: &mut Client, session_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let view = session_view(client, session_id);
        if view["goal"].is_null() {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "Goal was not cleared after completion: {view}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn string(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}
