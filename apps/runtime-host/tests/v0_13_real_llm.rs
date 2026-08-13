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

/// 显式人工入口：只读复制既有模型配置，所有 Runtime 数据和 Workspace 均位于临时目录。
#[test]
#[ignore = "uses the configured real Provider and may incur model charges"]
fn real_llm_delegates_parallel_read_only_tasks_and_parent_summarizes() {
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
    fs::write(
        workspace.path().join("alpha.txt"),
        "alpha-source-token: delegation-read-only-a",
    )
    .expect("write first read-only fixture");
    fs::write(
        workspace.path().join("beta.txt"),
        "beta-source-token: delegation-read-only-b",
    )
    .expect("write second read-only fixture");

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    assert_eq!(
        client.runtime("get_config_status", json!({}))["status"]["state"],
        "ready"
    );
    let workspace_id = text(
        &client.runtime("register_workspace", json!({ "path": workspace.path() }))["workspace"]["workspace_id"],
    );
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({
                "title": "v0.13 real delegation smoke",
                "workspace_id": workspace_id,
            }),
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
            "message": "Use exactly two delegate_task calls in one model turn. Delegate one child to read alpha.txt and report its token, and the other child to read beta.txt and report its token. Do not read the files in the parent. After both children return, summarize both tokens in the final answer.",
            "variant": "build",
            "idempotency_key": "real-v013-parallel-delegation"
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(run["status"], "completed");
    let tasks = client.runtime(
        "list_child_tasks",
        json!({ "session_id": session_id, "parent_run_id": run_id }),
    )["tasks"]
        .as_array()
        .expect("child tasks")
        .clone();
    assert_eq!(
        tasks.len(),
        2,
        "real LLM did not create exactly two child tasks"
    );
    assert!(tasks.iter().all(|task| task["status"] == "completed"));
    let final_text = run["text"].as_str().expect("parent final text");
    assert!(final_text.contains("delegation-read-only-a"));
    assert!(final_text.contains("delegation-read-only-b"));

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
    assert!(!logs.contains("delegation-read-only-a"));
    assert!(!logs.contains("delegation-read-only-b"));
    assert!(!logs.contains("Use exactly two delegate_task calls"));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a model credential"
        );
    }
}

fn wait_for_terminal(client: &mut support::Client, session_id: &str, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let run = client.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id }),
        )["run"]
            .clone();
        if matches!(
            run["status"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "compaction_required")
        ) {
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
