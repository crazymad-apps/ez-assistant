#![cfg(unix)]

mod support;

use serde_json::json;
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_config};

#[test]
fn parallel_children_survive_archive_restore_and_formal_host_restart() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "parallel-restart-secret",
    );

    let first_host = HostProcess::start(runtime_home.path());
    let mut first = first_host.connect();
    let session_id = first.runtime(
        "create_session",
        json!({ "title": "Parallel child restart", "model_key": "fixture" }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    first.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let submitted = first.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "DELEGATE_PARALLEL_CASE",
            "variant": "build",
            "idempotency_key": "parallel-child-restart"
        }),
    );
    let parent_run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("parent run id")
        .to_owned();
    assert_eq!(
        first.wait_for_status(
            "parallel delegated parent",
            &session_id,
            &parent_run_id,
            &["completed"]
        )["status"],
        "completed"
    );
    let before = first.runtime(
        "list_child_tasks",
        json!({ "session_id": session_id, "parent_run_id": parent_run_id }),
    )["tasks"]
        .as_array()
        .expect("child tasks")
        .clone();
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|task| task["status"] == "completed"));

    assert_eq!(
        first.runtime("archive_session", json!({ "session_id": session_id }))["session"]["lifecycle"],
        "archived"
    );
    let archived = first.runtime(
        "list_child_tasks",
        json!({ "session_id": session_id, "parent_run_id": parent_run_id }),
    )["tasks"]
        .clone();
    assert_eq!(archived, json!(before));
    assert_eq!(
        first.runtime("restore_session", json!({ "session_id": session_id }))["session"]["lifecycle"],
        "active"
    );
    first.runtime("shutdown_runtime", json!({}));
    drop(first);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(runtime_home.path());
    let mut second = second_host.connect();
    let recovered = second.runtime(
        "list_child_tasks",
        json!({ "session_id": session_id, "parent_run_id": parent_run_id }),
    )["tasks"]
        .clone();
    assert_eq!(recovered, json!(before));
    for task in before {
        let child_id = task["child_task_id"].as_str().expect("child id");
        assert_eq!(
            second.child_conversation(&session_id, child_id)["messages"]
                .as_array()
                .expect("child conversation")
                .len(),
            2
        );
    }
    second.runtime("shutdown_runtime", json!({}));
    drop(second);
    assert!(second_host.wait().status.success());
}

#[test]
fn killed_host_interrupts_child_and_repairs_the_parent_delegate_result_without_replay() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "child-interruption-secret",
    );

    let first_host = HostProcess::start(runtime_home.path());
    let mut first = first_host.connect();
    let session_id = first.runtime(
        "create_session",
        json!({ "title": "Interrupted child", "model_key": "fixture" }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    first.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let submitted = first.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "DELEGATE_BLOCK_CASE",
            "variant": "build",
            "idempotency_key": "interrupted-child-restart"
        }),
    );
    let parent_run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("parent run id")
        .to_owned();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let child_task_id = loop {
        let tasks = first.runtime(
            "list_child_tasks",
            json!({ "session_id": session_id, "parent_run_id": parent_run_id }),
        )["tasks"]
            .as_array()
            .expect("child tasks")
            .clone();
        if let Some(task) = tasks.first()
            && task["status"] == "running"
        {
            break task["child_task_id"].as_str().expect("child id").to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child did not reach running before kill"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    drop(first);
    let _ = first_host.kill();

    let second_host = HostProcess::start(runtime_home.path());
    let mut second = second_host.connect();
    let child = second.runtime(
        "get_child_task",
        json!({ "session_id": session_id, "child_task_id": child_task_id }),
    )["task"]
        .clone();
    assert_eq!(child["status"], "interrupted");
    let parent = second.runtime(
        "get_run",
        json!({ "session_id": session_id, "run_id": parent_run_id }),
    )["run"]
        .clone();
    assert_eq!(parent["status"], "interrupted");
    let conversation = second.conversation(&session_id);
    let messages = conversation["messages"]
        .as_array()
        .expect("parent messages");
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["turn"]["result"]["call_id"]
                .as_str()
                .is_some_and(|call_id| call_id.starts_with("call-delegate-"))
            && message["turn"]["result"]["status"] == "error"
    }));
    second.runtime("shutdown_runtime", json!({}));
    drop(second);
    assert!(second_host.wait().status.success());
}

/// 只验证 M4 新增的正式 HTTP 薄适配；复杂恢复故障点由 storage 单测覆盖，
/// Web Demo 展开与真实 Provider 回证留在 M5。
#[test]
fn child_query_cancel_and_private_conversation_use_the_formal_host_contract() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "child-observation-secret",
    );

    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let session_id = client.runtime(
        "create_session",
        json!({
            "title": "Child observation",
            "model_key": "fixture"
        }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "DELEGATE_CASE",
            "variant": "build",
            "idempotency_key": "formal-child-observation"
        }),
    );
    let parent_run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("parent run id")
        .to_owned();
    assert_eq!(
        client.wait_for_status(
            "delegated parent",
            &session_id,
            &parent_run_id,
            &["completed"]
        )["status"],
        "completed"
    );

    let tasks = client.runtime(
        "list_child_tasks",
        json!({
            "session_id": session_id,
            "parent_run_id": parent_run_id
        }),
    )["tasks"]
        .as_array()
        .expect("child task list")
        .clone();
    assert_eq!(tasks.len(), 1);
    let child_task_id = tasks[0]["child_task_id"]
        .as_str()
        .expect("child task id")
        .to_owned();
    assert_eq!(tasks[0]["status"], "completed");
    assert_eq!(tasks[0]["final_text"], "offline answer");

    let queried = client.runtime(
        "get_child_task",
        json!({
            "session_id": session_id,
            "child_task_id": child_task_id
        }),
    )["task"]
        .clone();
    assert_eq!(queried, tasks[0]);
    let conversation = client.child_conversation(&session_id, &child_task_id);
    assert_eq!(
        conversation["messages"]
            .as_array()
            .expect("child messages")
            .len(),
        2
    );

    // 终态取消是幂等查询，不改写已经可靠完成的状态。
    let cancelled = client.runtime(
        "cancel_child_task",
        json!({
            "session_id": session_id,
            "child_task_id": child_task_id
        }),
    );
    assert_eq!(cancelled["task"]["status"], "completed");
    assert_eq!(cancelled["task"]["cancel_requested"], false);

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}
