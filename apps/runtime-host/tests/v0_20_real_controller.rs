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

/// 显式人工入口：只读复制既有模型配置，在隔离 Runtime Home 中验证普通 Session
/// 的活动 Run 开启代理后会向自动主控可靠报告，并由主控模型形成用户可见回复。
#[test]
#[ignore = "uses the configured real Provider and may incur model charges"]
fn configured_model_processes_a_proxy_report_in_the_controller_session() {
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
    assert_eq!(
        client.runtime("get_config_status", json!({}))["status"]["state"],
        "ready"
    );
    let controller_session_id = wait_for_controller(&mut client);
    let target_session_id = text(
        &client.runtime(
            "create_session",
            json!({ "title": "v0.20 real managed session" }),
        )["session"]["session_id"],
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": target_session_id,
            "message": "Call update_plan exactly once with objective 'Validate controller proxy reporting' and one completed item named 'Prepare report'. After the tool result, reply with exactly: managed session completed",
            "variant": "build",
            "idempotency_key": "real-v020-managed-source"
        }),
    );
    let source_run_id = text(&submitted["run"]["run_id"]);
    client.runtime(
        "set_session_proxy",
        json!({ "session_id": target_session_id, "enabled": true }),
    );
    let source = client.wait_for_status(
        "managed source Run",
        &target_session_id,
        &source_run_id,
        &["completed", "failed", "cancelled"],
    );
    assert_eq!(source["status"], "completed", "source Run failed: {source}");

    let report_run = wait_for_controller_report(&mut client, &controller_session_id);
    assert_eq!(
        report_run["status"], "completed",
        "controller report Run failed: {report_run}"
    );
    assert!(
        report_run["text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty()),
        "controller did not produce a user-facing reply"
    );
    let view = client.runtime(
        "get_session_view",
        json!({ "session_id": controller_session_id }),
    )["snapshot"]["value"]
        .clone();
    assert!(
        view["conversation"]["items"]
            .as_array()
            .expect("controller conversation items")
            .iter()
            .any(|item| item["type"] == "user"
                && item["source"]["type"] == "proxy_report"
                && item["source"]["source_run_id"] == source_run_id),
        "controller conversation has no projected proxy report: {view}"
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
    assert!(!logs.contains("Validate controller proxy reporting"));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a credential"
        );
    }
}

fn wait_for_controller(client: &mut support::Client) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let sessions = client.runtime("list_sessions", json!({}))["sessions"]
            .as_array()
            .expect("session list")
            .clone();
        if let Some(controller) = sessions
            .iter()
            .find(|session| session["role"] == "controller")
        {
            return text(&controller["session_id"]);
        }
        assert!(Instant::now() < deadline, "controller creation timed out");
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_controller_report(client: &mut support::Client, session_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let runs = client.runtime("list_runs", json!({ "session_id": session_id }))["runs"]
            .as_array()
            .expect("controller Run list")
            .clone();
        if let Some(run) = runs.first() {
            let run_id = text(&run["run_id"]);
            let detail = client.runtime(
                "get_run",
                json!({ "session_id": session_id, "run_id": run_id }),
            )["run"]
                .clone();
            if detail["status"]
                .as_str()
                .is_some_and(|status| matches!(status, "completed" | "failed" | "cancelled"))
            {
                return detail;
            }
        }
        assert!(Instant::now() < deadline, "controller report timed out");
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
