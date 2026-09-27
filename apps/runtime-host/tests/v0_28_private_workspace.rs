//! 仅使用隔离 Runtime Home 的正式 Host 工作空间执行与冻结环境验收。
#![cfg(unix)]

mod support;

use std::{fs, path::Path};

use serde_json::{Value, json};

use support::{Client, FakeProvider, HostProcess, write_config};

#[test]
fn private_workspace_runs_from_managed_directory_across_edit_clear_restore_and_restart() {
    let provider = FakeProvider::start();
    let runtime_test_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&runtime_test_root).expect("prepare isolated Runtime test root");
    let home = tempfile::tempdir_in(runtime_test_root).expect("isolated Runtime Home");
    let external = support::test_directory();
    let replacement = support::test_directory();
    let external_directory = external
        .path()
        .canonicalize()
        .expect("canonical external Workspace directory")
        .to_string_lossy()
        .into_owned();
    write_config(
        home.path(),
        provider.endpoint(),
        "private-workspace-fixture",
    );

    let first_host = HostProcess::start(home.path());
    let mut client = first_host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({"label":"private fixture","primary_directory":null,"additional_directories":[]}),
    )["workspace"]
        .clone();
    let workspace_id = text(&workspace["workspace_id"]);
    let agent_directory = text(&workspace["agent_directory"]);
    assert!(workspace["user_directory"].is_null());
    assert!(Path::new(&agent_directory).is_dir());

    fs::write(
        Path::new(&agent_directory).join("AGENTS.md"),
        "Private fixture instructions",
    )
    .expect("write private Workspace instructions");
    let skill_directory = Path::new(&agent_directory).join(".agents/skills/private-check");
    fs::create_dir_all(&skill_directory).expect("create private Workspace Skill");
    fs::write(
        skill_directory.join("SKILL.md"),
        "---\nname: private-check\ndescription: Private Workspace Skill\n---\nUse private workspace files.\n",
    )
    .expect("write private Workspace Skill");
    let skills = client.runtime("list_skills", json!({"workspace_id":workspace_id}));
    assert!(
        skills["snapshot"]["skills"]
            .as_array()
            .expect("skill list")
            .iter()
            .any(|skill| skill["name"] == "private-check" && skill["health"] == "ready")
    );

    let old_session = text(
        &client.runtime(
            "create_session",
            json!({"workspace_id":workspace_id,"title":"frozen private","model_selection":null}),
        )["session"]["session_id"],
    );
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":old_session,"approval_mode":"auto"}),
    );
    assert_eq!(
        session_workspace(&mut client, &old_session)["primary_directory"],
        agent_directory
    );
    submit(&mut client, &old_session, "WRITE_CASE", "private-write");
    let written = Path::new(&agent_directory).join("default-permission-write.txt");
    assert_eq!(
        fs::read_to_string(&written).expect("private Workspace file tool output"),
        "written by default workspace permission"
    );
    submit(
        &mut client,
        &old_session,
        "PRIVATE_SHELL_CASE",
        "private-shell",
    );
    assert_eq!(
        fs::read_to_string(Path::new(&agent_directory).join("shell-cwd.txt"))
            .expect("private Workspace Shell cwd")
            .trim(),
        agent_directory
    );

    client.runtime(
        "update_workspace",
        json!({"workspace_id":workspace_id,"label":"external now","primary_directory":external.path(),"additional_directories":[]}),
    );
    assert_eq!(
        session_workspace(&mut client, &old_session)["primary_directory"],
        agent_directory
    );
    let new_session = text(
        &client.runtime(
            "create_session",
            json!({"workspace_id":workspace_id,"title":"new external","model_selection":null}),
        )["session"]["session_id"],
    );
    assert_eq!(
        session_workspace(&mut client, &new_session)["primary_directory"],
        external_directory
    );
    fs::remove_file(&written).expect("remove test marker before frozen-root check");
    submit(
        &mut client,
        &old_session,
        "WRITE_CASE",
        "old-session-still-private",
    );
    assert!(written.exists());
    assert!(
        !external
            .path()
            .join("default-permission-write.txt")
            .exists()
    );
    submit(
        &mut client,
        &new_session,
        "WRITE_CASE",
        "new-session-external",
    );
    assert!(
        external
            .path()
            .join("default-permission-write.txt")
            .exists()
    );

    let conversation = client.conversation(&old_session);
    let fork_point = conversation["items"]
        .as_array()
        .expect("conversation items")
        .iter()
        .find(|item| {
            item["type"] == "assistant"
                && item["segments"].as_array().is_some_and(|segments| {
                    segments.iter().any(|segment| segment["type"] == "text")
                })
        })
        .expect("assistant fork point")["message_id"]
        .clone();
    let fork_session = text(&client.runtime(
        "fork_session",
        json!({"session_id":old_session,"fork_point":fork_point,"expected_generation":conversation["generation"]}),
    )["session"]["session_id"]);
    assert_eq!(
        session_workspace(&mut client, &fork_session)["primary_directory"],
        agent_directory
    );
    fs::remove_file(&written).expect("remove test marker before fork check");
    submit(
        &mut client,
        &fork_session,
        "WRITE_CASE",
        "fork-still-private",
    );
    assert!(written.exists());

    client.runtime(
        "clear_session",
        json!({"session_id":old_session,"operation_id":"clear-frozen-private","expected_generation":conversation["generation"]}),
    );
    assert_eq!(
        session_workspace(&mut client, &old_session)["primary_directory"],
        agent_directory
    );

    client.runtime(
        "update_workspace",
        json!({"workspace_id":workspace_id,"label":"replacement now","primary_directory":replacement.path(),"additional_directories":[]}),
    );
    let replacement_session = text(
        &client.runtime(
            "create_session",
            json!({"workspace_id":workspace_id,"title":"new replacement","model_selection":null}),
        )["session"]["session_id"],
    );
    assert_eq!(
        session_workspace(&mut client, &replacement_session)["primary_directory"],
        replacement
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    client.runtime(
        "update_workspace",
        json!({"workspace_id":workspace_id,"label":"private again","primary_directory":null,"additional_directories":[]}),
    );
    let private_again = text(
        &client.runtime(
            "create_session",
            json!({"workspace_id":workspace_id,"title":"new private again","model_selection":null}),
        )["session"]["session_id"],
    );
    assert_eq!(
        session_workspace(&mut client, &private_again)["primary_directory"],
        agent_directory
    );
    assert_eq!(
        session_workspace(&mut client, &new_session)["primary_directory"],
        external_directory
    );
    fs::remove_file(external.path().join("default-permission-write.txt"))
        .expect("remove external marker before frozen-root check");
    submit(
        &mut client,
        &new_session,
        "WRITE_CASE",
        "external-session-still-frozen",
    );
    assert!(
        external
            .path()
            .join("default-permission-write.txt")
            .exists()
    );

    client.runtime("remove_workspace", json!({"workspace_id":workspace_id}));
    let restored = client.runtime("restore_workspace", json!({"workspace_id":workspace_id}));
    assert_eq!(restored["workspace"]["workspace_id"], workspace_id);
    assert!(restored["workspace"]["user_directory"].is_null());
    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(home.path());
    let mut client = second_host.connect();
    assert_eq!(
        session_workspace(&mut client, &old_session)["primary_directory"],
        agent_directory
    );
    submit(
        &mut client,
        &old_session,
        "PRIVATE_SHELL_CASE",
        "shell-after-restart",
    );
    assert_eq!(
        fs::read_to_string(Path::new(&agent_directory).join("shell-cwd.txt"))
            .expect("restarted private Workspace Shell cwd")
            .trim(),
        agent_directory
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(second_host.wait().status.success());
}

fn text(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}

fn session_workspace(client: &mut Client, session_id: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session_id}))["snapshot"]["value"]
        ["workspace"]
        .clone()
}

fn submit(client: &mut Client, session_id: &str, message: &str, key: &str) {
    let submitted = client.runtime(
        "submit_input",
        json!({"session_id":session_id,"message":message,"variant":"build","approval_mode":"auto","idempotency_key":key}),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status(message, session_id, &run_id, &["completed"])["status"],
        "completed"
    );
}
