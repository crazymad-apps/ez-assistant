#![cfg(unix)]

mod support;

use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use support::HostProcess;

/// 显式人工入口：只读复制既有模型配置，在隔离 Runtime Home 中验证真实模型能够
/// 先更新工作计划，再由 Runtime 跨 Run 自动续跑并通过 `update_goal` 可靠结束 Goal。
#[test]
#[ignore = "uses the configured real Provider and may incur model charges"]
fn configured_model_auto_continues_a_goal_across_runs() {
    let source_config = real_config_path();
    let config_text =
        fs::read_to_string(&source_config).expect("read explicitly configured real LLM config");
    let credentials = configured_credentials(&config_text);
    assert!(
        !credentials.is_empty(),
        "real LLM config has no model API key"
    );

    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    fs::write(runtime_home.path().join("config.toml"), config_text)
        .expect("copy config into isolated Runtime Home");

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({ "title": "v0.18 real Goal continuation smoke" }),
        )["session"]["session_id"],
    );
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "Run a deterministic Goal lifecycle smoke test. During the first Goal Run, call update_plan exactly once with objective 'Validate real Goal continuation' and one in_progress item named 'Complete the second Goal Run'. After the tool result, end the Run with a short ordinary final answer and do not call update_goal. When the Runtime automatically starts the next Goal Run, call update_goal as the only tool call with status complete and a short summary. Do not call any other tool.",
            "variant": "build",
            "mode": "start_goal",
            "idempotency_key": "real-v018-goal-continuation"
        }),
    );
    let first_run_id = text(&submitted["run"]["run_id"]);
    wait_for_goal_clear(&mut client, &session_id);

    let runs = client.runtime("list_runs", json!({ "session_id": session_id }))["runs"]
        .as_array()
        .expect("Run list")
        .clone();
    assert!(runs.len() >= 2, "real Goal did not cross a Run boundary");
    assert!(
        runs.iter().any(|run| run["run_id"] == first_run_id),
        "first Goal Run is missing"
    );

    let mut observed_update_plan = false;
    let mut observed_update_goal = false;
    for run in runs {
        let run_id = text(&run["run_id"]);
        let detail = client.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id }),
        )["run"]
            .clone();
        assert_eq!(detail["status"], "completed", "a Goal Run did not complete");
        for tool in detail["tools"].as_array().expect("tool activities") {
            observed_update_plan |=
                tool["tool_name"] == "update_plan" && tool["status"] == "completed";
            observed_update_goal |=
                tool["tool_name"] == "update_goal" && tool["status"] == "completed";
            assert_ne!(tool["tool_name"], "judge", "Goal must not use a Judge tool");
        }
    }
    assert!(observed_update_plan, "real model did not call update_plan");
    assert!(observed_update_goal, "real model did not call update_goal");

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    let output = host.wait();
    assert!(output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!logs.contains(&access_token));
    assert!(!logs.contains("Validate real Goal continuation"));
    assert!(!logs.contains("Run a deterministic Goal lifecycle smoke test"));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a model credential"
        );
    }
}

fn wait_for_goal_clear(client: &mut support::Client, session_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let view = client.runtime("get_session_view", json!({ "session_id": session_id }))["snapshot"]
            ["value"]
            .clone();
        if view["goal"].is_null() {
            return;
        }
        if view["goal"]["state"] == "paused" {
            panic!("real Goal paused before completion");
        }
        assert!(Instant::now() < deadline, "real Goal timed out");
        thread::sleep(Duration::from_millis(100));
    }
}

fn real_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("EZ_ASSISTANT_REAL_LLM_CONFIG") {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .expect("home directory; or set EZ_ASSISTANT_REAL_LLM_CONFIG")
        .join(".ez-assistant/config.toml")
}

fn configured_credentials(document: &str) -> Vec<String> {
    let parsed: toml::Value =
        toml::from_str(document).unwrap_or_else(|_| panic!("real LLM config could not be parsed"));
    parsed
        .get("models")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|models| models.values())
        .filter_map(|model| model.get("api_key"))
        .filter_map(toml::Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn text(value: &Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
