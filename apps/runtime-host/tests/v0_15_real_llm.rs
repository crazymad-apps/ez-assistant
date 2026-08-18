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

use support::{FakeProvider, HostProcess};

/// 显式人工入口：先用离线 Provider 在隔离 Runtime Home 写入历史会话，再只读复制
/// 既有模型配置，验证真实 Provider 能完成 Pinned Memory 与 Conversation Recall 工具闭环。
#[test]
#[ignore = "uses the configured real Provider and may incur model charges"]
fn real_llm_pins_memory_and_recalls_an_isolated_historical_conversation() {
    let source_config = real_config_path();
    let real_config =
        fs::read_to_string(&source_config).expect("read explicitly configured real LLM config");
    let credentials = configured_credentials(&real_config);
    assert!(
        !credentials.is_empty(),
        "real LLM config has no model API key"
    );

    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let marker_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after epoch")
        .as_nanos();
    let recall_marker = format!("v015-recall-marker-{marker_suffix}");
    let pinned_marker = format!("v015-pinned-marker-{marker_suffix}");

    seed_historical_conversation(runtime_home.path(), &recall_marker);
    fs::write(runtime_home.path().join("config.toml"), real_config)
        .expect("copy real config into isolated Runtime Home");

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    assert_eq!(
        client.runtime("get_config_status", json!({}))["status"]["state"],
        "ready"
    );
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({ "title": "v0.15 real memory and recall smoke" }),
        )["session"]["session_id"],
    );
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "ask" }),
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": format!(
                "This is a strict tool integration test. First call pin_memory exactly once with category user_preference and content {pinned_marker:?}. Then call recall_memory with action search, scope global, query {recall_marker:?}, and limit 5. Do not finish before both tool calls complete. In the final answer, repeat both marker strings exactly."
            ),
            "variant": "build",
            "idempotency_key": "real-v015-memory-recall"
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal_and_approve(&mut client, &session_id, &run_id);
    assert_eq!(run["status"], "completed", "real memory Run failed: {run}");
    let tools = run["tools"].as_array().expect("tool activities");
    assert!(
        tools
            .iter()
            .any(|tool| tool["tool_name"] == "pin_memory" && tool["status"] == "completed"),
        "real LLM did not complete pin_memory: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool["tool_name"] == "recall_memory" && tool["status"] == "completed"),
        "real LLM did not complete recall_memory: {tools:?}"
    );
    assert!(
        run["text"]
            .as_str()
            .is_some_and(|text| text.contains(&recall_marker) && text.contains(&pinned_marker)),
        "real LLM final answer omitted a verification marker"
    );

    let pinned = client.runtime("list_pinned_memories", json!({}));
    assert!(
        pinned["collection"]["items"]
            .as_array()
            .expect("Pinned Memory items")
            .iter()
            .any(|entry| entry["content"] == pinned_marker),
        "pin_memory result was not persisted"
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
    assert!(!logs.contains(&recall_marker));
    assert!(!logs.contains(&pinned_marker));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a model credential"
        );
    }
}

fn seed_historical_conversation(runtime_home: &std::path::Path, marker: &str) {
    let provider = FakeProvider::start();
    support::write_config(runtime_home, provider.endpoint(), "offline-secret");
    let host = HostProcess::start(runtime_home);
    let mut client = host.connect();
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({ "title": "v0.15 isolated recall seed" }),
        )["session"]["session_id"],
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": format!("Remember this historical test token: {marker}"),
            "variant": "build",
            "idempotency_key": "real-v015-recall-seed"
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id, Duration::from_secs(20));
    assert_eq!(run["status"], "completed");
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

fn wait_for_terminal_and_approve(
    client: &mut support::Client,
    session_id: &str,
    run_id: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let approvals = client.runtime(
            "list_pending_approvals",
            json!({ "session_id": session_id }),
        )["approvals"]
            .as_array()
            .expect("pending approvals")
            .clone();
        for approval in approvals {
            client.runtime(
                "decide_approval",
                json!({
                    "session_id": session_id,
                    "approval_id": approval["approval_id"],
                    "decision": "allow_once"
                }),
            );
        }
        let run = client.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id }),
        )["run"]
            .clone();
        if is_terminal(&run) {
            return run;
        }
        assert!(Instant::now() < deadline, "real LLM Run timed out");
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_terminal(
    client: &mut support::Client,
    session_id: &str,
    run_id: &str,
    timeout: Duration,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let run = client.runtime(
            "get_run",
            json!({ "session_id": session_id, "run_id": run_id }),
        )["run"]
            .clone();
        if is_terminal(&run) {
            return run;
        }
        assert!(Instant::now() < deadline, "Run timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

fn is_terminal(run: &Value) -> bool {
    matches!(
        run["status"].as_str(),
        Some("completed" | "failed" | "cancelled" | "interrupted" | "compaction_required")
    )
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
    // 解析失败时不能格式化含 credential 的完整 TOML，避免测试日志泄密。
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
