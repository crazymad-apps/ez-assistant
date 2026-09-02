#![cfg(unix)]

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::blocking::{Client as HttpClient, multipart};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::HostProcess;

/// 显式人工入口：只读现有模型配置，但把 Runtime 数据、Workspace 和附件全部放在临时目录。
#[test]
#[ignore = "uses the configured real Provider and may incur a small model charge"]
fn configured_model_reads_a_real_attachment_through_the_formal_host() {
    let source_config = real_config_path();
    let config_text =
        fs::read_to_string(&source_config).expect("read the explicitly configured real LLM config");
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
        "ez-assistant-real-file-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time after epoch")
            .as_nanos()
    );
    let source = workspace.path().join("reference.txt");
    fs::write(&source, &marker).expect("write isolated reference file");

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let model_key = text(&configuration["status"]["default_model"]);
    let validation = client.runtime(
        "validate_model_connection",
        json!({ "model_key": model_key }),
    );
    if validation["outcome"]["status"] != "succeeded" {
        panic!(
            "real LLM connection validation failed with safe kind {}",
            validation["outcome"]["failure"]["kind"]
                .as_str()
                .unwrap_or("unknown")
        );
    }
    let registered = client.runtime(
        "register_workspace",
        json!({
            "label": "attachment-workspace",
            "primary_directory": workspace.path(),
            "additional_directories": [],
        }),
    );
    let workspace_id = text(&registered["workspace"]["workspace_id"]);
    let created = client.runtime(
        "create_session",
        json!({
            "title": "v0.11 real file smoke",
            "workspace_id": workspace_id,
        }),
    );
    let session_id = text(&created["session"]["session_id"]);
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let attachment_id = upload(&host, &session_id, &source);
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "Use the read_file tool to read the attached file. Do not guess. Reply with only the exact file content.",
            "variant": "build",
            "attachment_ids": [attachment_id],
            "idempotency_key": "real-llm-file-smoke"
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    if run["status"] != "completed" {
        panic!(
            "real LLM Run ended as {} with safe error code {}",
            run["status"].as_str().unwrap_or("unknown"),
            run["error"]["code"].as_str().unwrap_or("none")
        );
    }
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| tool["tool_name"] == "read_file" && tool["status"] == "completed"),
        "real LLM did not complete read_file"
    );
    assert!(
        run["text"]
            .as_str()
            .is_some_and(|text| text.contains(&marker)),
        "real LLM response did not contain the isolated file marker"
    );

    client.runtime("shutdown_runtime", json!({}));
    let output = host.wait();
    assert!(output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!logs.contains(&access_token));
    assert!(!logs.contains(&marker));
    assert!(!logs.contains("Use the read_file tool"));
    for credential in credentials {
        assert!(
            !logs.contains(&credential),
            "Host output leaked a model credential"
        );
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
    // TOML parser errors can retain the complete source document. Never format that error because
    // this explicit smoke test reads a credential-bearing configuration.
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

fn upload(host: &HostProcess, session_id: &str, path: &Path) -> String {
    let response = HttpClient::new()
        .post(format!(
            "{}/sessions/{session_id}/attachments",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .multipart(
            multipart::Form::new()
                .file("file", path)
                .expect("build real LLM upload part"),
        )
        .send()
        .expect("upload real LLM attachment");
    let status = response.status();
    let body: Value = response.json().expect("real LLM upload JSON");
    assert!(status.is_success(), "real LLM upload failed");
    text(&body["attachment"]["attachment_id"])
}

fn wait_for_terminal(client: &mut support::Client, session_id: &str, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
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
            return run;
        }
        assert!(Instant::now() < deadline, "real LLM Run timed out");
        thread::sleep(Duration::from_millis(100));
    }
}

fn text(value: &Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
