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

#[test]
fn modes_permission_reload_and_injected_parts_survive_formal_host_restart() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "mode-reload-secret",
    );
    let workspace_directory = TempDir::new().expect("isolated user workspace");

    let first_host = HostProcess::start(runtime_home.path());
    let mut first = first_host.connect();
    let workspace_id = first.runtime(
        "register_workspace",
        json!({ "path": workspace_directory.path() }),
    )["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_owned();
    let session_id = first.runtime(
        "create_session",
        json!({
            "title": "Mode and reload recovery",
            "model_key": "fixture",
            "workspace_id": workspace_id,
        }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    first.runtime(
        "set_session_variant",
        json!({ "session_id": session_id, "variant": "plan" }),
    );
    first.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    assert!(
        first.conversation(&session_id)["messages"]
            .as_array()
            .expect("messages")
            .is_empty()
    );
    assert!(
        first.runtime("list_runs", json!({ "session_id": session_id }))["runs"]
            .as_array()
            .expect("runs")
            .is_empty()
    );
    first.runtime("shutdown_runtime", json!({}));
    drop(first);
    assert!(first_host.wait().status.success());

    let database = runtime_home.path().join("data/runtime.sqlite3");
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open temporary acceptance database read-only");
    let stored_modes: (String, String) = connection
        .query_row(
            "SELECT current_variant, approval_mode FROM sessions WHERE session_id = ?1",
            [&session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("stored session modes");
    assert_eq!(stored_modes, ("plan".to_owned(), "auto".to_owned()));
    drop(connection);

    let second_host = HostProcess::start(runtime_home.path());
    let mut second = second_host.connect();
    let recovered = second.runtime("get_session", json!({ "session_id": session_id }));
    assert_eq!(recovered["session"]["current_variant"], "plan");
    assert_eq!(recovered["session"]["approval_mode"], "auto");

    let valid_global = json!({
        "schema_version": 1,
        "rules": [{
            "id": "manual-global-deny",
            "effect": "deny",
            "variants": ["plan", "build"],
            "matcher": { "type": "general", "tool_name": "manual_fixture" }
        }]
    });
    fs::write(
        runtime_home.path().join("permissions.json"),
        serde_json::to_vec_pretty(&valid_global).expect("serialize valid permissions"),
    )
    .expect("write valid global permissions");
    let applied = second.runtime("reload_permissions", json!({ "session_id": session_id }));
    assert_eq!(applied["applied"], true);

    fs::write(
        runtime_home.path().join("permissions.json"),
        b"{ invalid strict json",
    )
    .expect("write invalid global permissions");
    let rejected = second.runtime("reload_permissions", json!({ "session_id": session_id }));
    assert_eq!(rejected["applied"], false);
    assert!(
        rejected["diagnostics"]
            .as_array()
            .expect("reload diagnostics")
            .iter()
            .any(|diagnostic| diagnostic["scope"] == "global")
    );

    fs::write(
        runtime_home.path().join("permissions.json"),
        serde_json::to_vec_pretty(&valid_global).expect("serialize restored permissions"),
    )
    .expect("restore valid global permissions");
    assert_eq!(
        second.runtime("reload_permissions", json!({ "session_id": session_id }))["applied"],
        true
    );

    let submitted = second.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "DEFAULT_CASE",
            "variant": "build",
            "idempotency_key": "build-overrides-recovered-plan"
        }),
    );
    let run_id = submitted["run"]["run_id"].as_str().expect("run id");
    assert_eq!(submitted["run"]["variant"], "build");
    assert_eq!(
        second.wait_for_status("build input", &session_id, run_id, &["completed"])["status"],
        "completed"
    );
    let conversation = second.conversation(&session_id);
    let parts = conversation["messages"][0]["turn"]["parts"]
        .as_array()
        .expect("User Message parts");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["data"]["text"], "DEFAULT_CASE");
    assert_eq!(parts[1]["type"], "injected");
    assert!(
        parts[1]["data"]["text"]
            .as_str()
            .expect("injected text")
            .contains("mode=\"build\"")
    );
    assert_eq!(
        second.runtime("get_session", json!({ "session_id": session_id }))["session"]["current_variant"],
        "build"
    );

    second.runtime("shutdown_runtime", json!({}));
    drop(second);
    assert!(second_host.wait().status.success());
}

#[test]
fn workspace_allow_is_variant_scoped_in_the_formal_host() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "variant-scope-secret",
    );
    let workspace_directory = TempDir::new().expect("isolated user workspace");
    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({ "path": workspace_directory.path() }),
    )["workspace"]
        .clone();
    let workspace_id = workspace["workspace_id"].as_str().expect("workspace id");
    let agent_directory = workspace["agent_directory"]
        .as_str()
        .expect("Agent private directory")
        .to_owned();
    let session_id = client.runtime(
        "create_session",
        json!({
            "title": "Variant scoped workspace allow",
            "model_key": "fixture",
            "workspace_id": workspace_id,
        }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();

    let build = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "TOOL_CASE",
            "variant": "build",
            "idempotency_key": "workspace-build"
        }),
    );
    let build_run = build["run"]["run_id"].as_str().expect("build run id");
    let build_approval = wait_for_pending_approval(&mut client, &session_id);
    assert!(
        build_approval["available_decisions"]
            .as_array()
            .expect("decisions")
            .contains(&json!("allow_workspace"))
    );
    client.runtime(
        "decide_approval",
        json!({
            "session_id": session_id,
            "approval_id": build_approval["approval_id"],
            "decision": "allow_workspace"
        }),
    );
    assert_eq!(
        client.wait_for_status("Build tool", &session_id, build_run, &["completed"])["status"],
        "completed"
    );
    let permission_path = std::path::Path::new(&agent_directory).join("permissions.json");
    let permission_document: Value =
        serde_json::from_slice(&fs::read(&permission_path).expect("read Workspace permissions"))
            .expect("parse Workspace permissions");
    assert_eq!(
        permission_document["rules"][0]["variants"],
        json!(["build"])
    );

    let plan = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "TOOL_CASE",
            "variant": "plan",
            "idempotency_key": "workspace-plan"
        }),
    );
    let plan_run = plan["run"]["run_id"].as_str().expect("plan run id");
    let plan_approval = wait_for_pending_approval(&mut client, &session_id);
    assert_eq!(plan_approval["variant"], "plan");
    client.runtime(
        "decide_approval",
        json!({
            "session_id": session_id,
            "approval_id": plan_approval["approval_id"],
            "decision": "deny"
        }),
    );
    assert_eq!(
        client.wait_for_status("Plan denied tool", &session_id, plan_run, &["completed"])["status"],
        "completed"
    );
    let unchanged: Value = serde_json::from_slice(
        &fs::read(permission_path).expect("read unchanged Workspace permissions"),
    )
    .expect("parse unchanged Workspace permissions");
    assert_eq!(unchanged["rules"].as_array().expect("rules").len(), 1);
    assert_eq!(unchanged["rules"][0]["variants"], json!(["build"]));

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

#[test]
fn pending_approval_is_queryable_but_is_not_restored_after_host_restart() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "approval-recovery-secret",
    );

    let first_host = HostProcess::start(runtime_home.path());
    let mut client = first_host.connect();
    let session_id = client.runtime(
        "create_session",
        json!({ "title": "Approval restart", "model_key": "fixture" }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let run_id = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "TOOL_CASE",
            "variant": "build",
            "idempotency_key": "approval-before-restart"
        }),
    )["run"]["run_id"]
        .as_str()
        .expect("run id")
        .to_owned();
    let pending = wait_for_pending_approval(&mut client, &session_id);
    assert_eq!(pending["run_id"], run_id);
    assert_eq!(pending["approval_mode"], "ask");
    assert_eq!(pending["subject"]["tool_name"], "list_directory");

    drop(client);
    let _ = first_host.kill();

    let second_host = HostProcess::start(runtime_home.path());
    let mut recovered = second_host.connect();
    assert!(
        recovered.runtime(
            "list_pending_approvals",
            json!({ "session_id": session_id })
        )["approvals"]
            .as_array()
            .expect("approval list")
            .is_empty()
    );
    assert_eq!(
        recovered.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id })
        )["run"]["status"],
        "interrupted"
    );
    recovered.runtime("shutdown_runtime", json!({}));
    drop(recovered);
    assert!(second_host.wait().status.success());
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
