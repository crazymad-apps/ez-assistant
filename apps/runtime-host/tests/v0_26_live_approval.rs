#![cfg(unix)]

mod support;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

#[test]
fn approval_mode_changes_affect_later_tools_in_the_same_formal_host_run() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "live-approval-secret",
    );
    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let session_id = create_session_without_implicit_permissions(&mut client, runtime_home.path());
    let run_id = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "TOOL_TWICE_CASE",
            "variant": "build",
            "idempotency_key": "live-parent-approval"
        }),
    )["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let pending = wait_for_pending_approval(&mut client, &session_id);

    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    assert_eq!(pending_approval_count(&mut client, &session_id), 1);
    client.runtime(
        "decide_approval",
        json!({
            "session_id": session_id,
            "approval_id": pending["approval_id"],
            "decision": "allow_once"
        }),
    );

    assert_eq!(
        client.wait_for_status(
            "two live-authorized tools",
            &session_id,
            &run_id,
            &["completed"]
        )["status"],
        "completed"
    );
    assert_eq!(pending_approval_count(&mut client, &session_id), 0);
    let conversation = client.conversation(&session_id).to_string();
    assert!(conversation.contains("call-list-directory-1"));
    assert!(conversation.contains("call-list-directory-2"));

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

#[test]
fn delegated_child_reads_the_parent_sessions_current_mode_in_the_formal_host() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "live-child-approval-secret",
    );
    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let session_id = create_session_without_implicit_permissions(&mut client, runtime_home.path());
    let run_id = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "DELEGATE_PROBE_CASE",
            "variant": "build",
            "idempotency_key": "live-child-approval"
        }),
    )["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let parent_pending = wait_for_pending_approval(&mut client, &session_id);
    assert!(parent_pending["child_task_id"].is_null());

    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    client.runtime(
        "decide_approval",
        json!({
            "session_id": session_id,
            "approval_id": parent_pending["approval_id"],
            "decision": "allow_once"
        }),
    );

    assert_eq!(
        client.wait_for_status(
            "delegated live approval",
            &session_id,
            &run_id,
            &["completed"]
        )["status"],
        "completed"
    );
    assert_eq!(pending_approval_count(&mut client, &session_id), 0);
    let tasks = client.runtime(
        "list_child_tasks",
        json!({ "session_id": session_id, "parent_run_id": run_id }),
    )["tasks"]
        .as_array()
        .expect("child tasks")
        .clone();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["status"], "completed");

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

fn create_session_without_implicit_permissions(
    client: &mut Client,
    home: &std::path::Path,
) -> String {
    let session_id = client.runtime(
        "create_session",
        json!({ "title": "Live approval", "model_selection": null }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    fs::write(
        home.join("data/sessions")
            .join(&session_id)
            .join("private/permissions.json"),
        b"{\"schema_version\":1,\"rules\":[]}",
    )
    .expect("remove implicit session permission");
    assert_eq!(
        client.runtime("reload_permissions", json!({ "session_id": session_id }))["applied"],
        true
    );
    session_id
}

fn wait_for_pending_approval(client: &mut Client, session_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let approvals = client.runtime(
            "list_pending_approvals",
            json!({ "session_id": session_id }),
        )["approvals"]
            .as_array()
            .expect("approval list")
            .clone();
        if let Some(approval) = approvals.into_iter().next() {
            return approval;
        }
        assert!(Instant::now() < deadline, "approval did not become pending");
        thread::sleep(Duration::from_millis(20));
    }
}

fn pending_approval_count(client: &mut Client, session_id: &str) -> usize {
    client.runtime(
        "list_pending_approvals",
        json!({ "session_id": session_id }),
    )["approvals"]
        .as_array()
        .expect("approval list")
        .len()
}
