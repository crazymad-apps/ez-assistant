#![cfg(unix)]

mod support;

use std::fs;

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

const TEST_API_KEY: &str = "offline-skill-secret-must-not-leak";

#[test]
fn formal_host_freezes_user_skill_activation_across_disable_fork_and_restart() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated Workspace");
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);
    let skill_directory = workspace.path().join(".ez-assistant/skills/review");
    fs::create_dir_all(&skill_directory).expect("create skill directory");
    fs::write(
        skill_directory.join("SKILL.md"),
        "---\nname: review\ndescription: Review changes carefully\nuser-invocable: true\n---\nUse the frozen review instructions.\n",
    )
    .expect("write Skill definition");

    let first_host = HostProcess::start(runtime_home.path());
    let mut client = first_host.connect();
    let workspace_id = string(
        &client.runtime("register_workspace", json!({"path": workspace.path()}))["workspace"]["workspace_id"],
    );
    let listed = client.runtime("list_skills", json!({"workspace_id":workspace_id}));
    assert_eq!(skill(&listed, "review")["health"], "ready");
    let detail = client.runtime(
        "get_skill_detail",
        json!({"workspace_id":workspace_id,"name":"review"}),
    );
    assert_eq!(
        detail["detail"]["skill"]["description"],
        "Review changes carefully"
    );
    assert_eq!(
        detail["detail"]["body"],
        "Use the frozen review instructions.\n"
    );
    let session_id = string(
        &client.runtime(
            "create_session",
            json!({
                "title":"Skill activation",
                "model_key":"fixture",
                "workspace_id":workspace_id
            }),
        )["session"]["session_id"],
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":"SKILL_FORMAL_CASE",
            "variant":"build",
            "skill_name":"review",
            "idempotency_key":"skill-formal-input"
        }),
    );
    let run_id = string(&submitted["run"]["run_id"]);
    assert_eq!(
        client.wait_for_status("Skill activation Run", &session_id, &run_id, &["completed"])["status"],
        "completed"
    );
    let view = session_view(&mut client, &session_id);
    assert_eq!(view["skill_catalog"]["skills"][0]["name"], "review");
    assert_eq!(view["active_skills"][0]["tag"]["name"], "review");
    assert_eq!(view["conversation"]["items"][0]["skill"]["name"], "review");

    let disabled = client.runtime(
        "set_skill_enabled",
        json!({"workspace_id":workspace_id,"name":"review","enabled":false}),
    );
    assert_eq!(skill(&disabled, "review")["health"], "disabled");
    assert_eq!(
        session_view(&mut client, &session_id)["active_skills"][0]["tag"]["name"],
        "review",
        "current discovery changes must not rewrite an accepted Activation"
    );

    let assistant_message_id = view["conversation"]["items"]
        .as_array()
        .expect("conversation items")
        .iter()
        .find(|item| item["type"] == "assistant")
        .map(|item| string(&item["message_id"]))
        .expect("assistant message");
    let forked_session_id = string(
        &client.runtime(
            "fork_session",
            json!({
                "session_id":session_id,
                "fork_point":assistant_message_id,
                "expected_generation":view["conversation"]["generation"]
            }),
        )["session"]["session_id"],
    );
    assert_eq!(
        session_view(&mut client, &forked_session_id)["active_skills"][0]["tag"]["name"],
        "review"
    );

    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    assert_eq!(
        session_view(&mut client, &session_id)["active_skills"][0]["tag"]["name"],
        "review"
    );
    assert_eq!(
        skill(
            &client.runtime("list_skills", json!({"workspace_id":workspace_id})),
            "review"
        )["health"],
        "disabled"
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(second_host.wait().status.success());

    let connection = Connection::open_with_flags(
        runtime_home.path().join("data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open database read-only");
    let activation_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM skill_activations", [], |row| {
            row.get(0)
        })
        .expect("activation count");
    assert_eq!(
        activation_count, 2,
        "source and Fork each own one ledger fact"
    );
    let frozen_input_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM inputs WHERE skill_activation_json IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("frozen input count");
    assert_eq!(frozen_input_count, 1);
}

#[test]
fn formal_host_loads_model_skill_and_continues_same_run_with_monotonic_steps() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated Workspace");
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);
    let skill_directory = workspace.path().join(".ez-assistant/skills/review");
    fs::create_dir_all(&skill_directory).expect("create skill directory");
    fs::write(
        skill_directory.join("SKILL.md"),
        "---\nname: review\ndescription: Review changes carefully\n---\nUse the frozen review instructions.\n",
    )
    .expect("write Skill definition");

    let first_host = HostProcess::start(runtime_home.path());
    let mut client = first_host.connect();
    let workspace_id = string(
        &client.runtime("register_workspace", json!({"path": workspace.path()}))["workspace"]["workspace_id"],
    );
    let session_id = string(
        &client.runtime(
            "create_session",
            json!({
                "title":"Agent Skill activation",
                "model_key":"fixture",
                "workspace_id":workspace_id
            }),
        )["session"]["session_id"],
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":"SKILL_AGENT_CASE",
            "variant":"build",
            "idempotency_key":"skill-agent-input"
        }),
    );
    let run_id = string(&submitted["run"]["run_id"]);
    let run = client.wait_for_status("Agent Skill Run", &session_id, &run_id, &["completed"]);
    assert_eq!(run["text"], "agent skill applied");
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| { tool["tool_name"] == "load_skill" && tool["status"] == "completed" }),
        "load_skill did not complete: {run}"
    );
    let view = session_view(&mut client, &session_id);
    assert_eq!(view["active_skills"][0]["tag"]["name"], "review");
    assert_eq!(view["active_skills"][0]["trigger"], "model");

    assert_eq!(
        client.runtime("shutdown_runtime", json!({}))["lifecycle"],
        "stopped"
    );
    drop(client);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(runtime_home.path());
    let mut client = second_host.connect();
    let recovered = session_view(&mut client, &session_id);
    assert_eq!(recovered["active_skills"][0]["tag"]["name"], "review");
    assert_eq!(recovered["active_skills"][0]["trigger"], "model");
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(second_host.wait().status.success());

    let connection = Connection::open_with_flags(
        runtime_home.path().join("data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open database read-only");
    let trigger: String = connection
        .query_row(
            "SELECT trigger FROM skill_activations WHERE session_id = ?1",
            [&session_id],
            |row| row.get(0),
        )
        .expect("model activation trigger");
    assert_eq!(trigger, "model");
    let steps = connection
        .prepare(
            "SELECT DISTINCT step FROM run_message_refs
             WHERE run_id = ?1 AND step IS NOT NULL ORDER BY step",
        )
        .expect("prepare run step query")
        .query_map([&run_id], |row| row.get::<_, u32>(0))
        .expect("query run steps")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect run steps");
    assert_eq!(steps, vec![1, 2]);
}

fn session_view(client: &mut Client, session_id: &str) -> Value {
    client.runtime("get_session_view", json!({"session_id":session_id}))["snapshot"]["value"]
        .clone()
}

fn skill<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["snapshot"]["skills"]
        .as_array()
        .expect("Skill summaries")
        .iter()
        .find(|skill| skill["name"] == name)
        .expect("named Skill")
}

fn string(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}
