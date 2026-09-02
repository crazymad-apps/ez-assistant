#![cfg(unix)]

mod support;

use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use support::HostProcess;

/// 显式人工入口：读取既有模型配置，但把 Runtime 数据和 Workspace 完全隔离到临时目录。
#[test]
#[ignore = "uses the configured real Provider and may incur model charges"]
fn real_llm_completes_plan_then_approved_build_shell_in_the_formal_host() {
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
    let workspace = TempDir::new().expect("isolated user Workspace");
    let marker = format!(
        "ez-assistant-v012-shell-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time after epoch")
            .as_nanos()
    );

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let registered = client.runtime(
        "register_workspace",
        json!({
            "label": "approval-real-llm",
            "primary_directory": workspace.path(),
            "additional_directories": [],
        }),
    );
    let workspace_id = text(&registered["workspace"]["workspace_id"]);
    let created = client.runtime(
        "create_session",
        json!({
            "title": "v0.12 real Plan Build smoke",
            "workspace_id": workspace_id,
        }),
    );
    let session_id = text(&created["session"]["session_id"]);

    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let planned = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "Inspect the current workspace with file tools if useful. Produce a short implementation plan only; do not implement or modify the user workspace.",
            "variant": "plan",
            "idempotency_key": "real-v012-plan"
        }),
    );
    let plan_run_id = text(&planned["run"]["run_id"]);
    let plan_run = wait_for_terminal(&mut client, &session_id, &plan_run_id, false);
    assert_eq!(plan_run["status"], "completed");

    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "ask" }),
    );
    let output_path = workspace.path().join("v012-shell-smoke.txt");
    let command = format!("printf '%s' '{}' > '{}'", marker, output_path.display());
    let built = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": format!(
                "Start implementation. You must use the shell tool to run exactly this command, then report completion: {command}"
            ),
            "variant": "build",
            "idempotency_key": "real-v012-build-shell"
        }),
    );
    let build_run_id = text(&built["run"]["run_id"]);
    let build_run = wait_for_terminal(&mut client, &session_id, &build_run_id, true);
    assert_eq!(build_run["status"], "completed");
    assert!(
        build_run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| tool["tool_name"] == "shell" && tool["status"] == "completed"),
        "real LLM did not complete an approved shell call"
    );
    assert_eq!(
        fs::read_to_string(&output_path).expect("read shell-created file"),
        marker
    );

    let conversation = client.conversation(&session_id);
    let messages = conversation["items"]
        .as_array()
        .expect("Conversation items");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["type"] == "user")
            .count(),
        2
    );
    assert!(
        messages
            .iter()
            .filter(|message| message["type"] == "assistant")
            .count()
            >= 2
    );

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
    assert!(!logs.contains(&marker));
    assert!(!logs.contains("You must use the shell tool"));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a model credential"
        );
    }
}

fn wait_for_terminal(
    client: &mut support::Client,
    session_id: &str,
    run_id: &str,
    approve_pending: bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut observed_shell_approval = false;
    loop {
        if approve_pending {
            let approvals = client.runtime(
                "list_pending_approvals",
                json!({ "session_id": session_id }),
            )["approvals"]
                .as_array()
                .expect("pending approvals")
                .clone();
            for approval in approvals {
                observed_shell_approval |= approval["subject"]["type"] == "shell";
                client.runtime(
                    "decide_approval",
                    json!({
                        "session_id": session_id,
                        "approval_id": approval["approval_id"],
                        "decision": "allow_once"
                    }),
                );
            }
        }
        let run = client.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id }),
        )["run"]
            .clone();
        let status = run["status"].as_str().expect("Run status");
        if matches!(
            status,
            "completed" | "failed" | "cancelled" | "interrupted" | "compaction_required"
        ) {
            if approve_pending {
                assert!(
                    observed_shell_approval,
                    "real LLM did not request Shell approval"
                );
            }
            return run;
        }
        assert!(Instant::now() < deadline, "real LLM Run timed out");
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
    // 解析器错误可能携带完整源文本；这里绝不格式化 credential-bearing TOML 的错误。
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
