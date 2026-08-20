#![cfg(unix)]

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use image::{Rgb, RgbImage};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::HostProcess;

/// 显式人工入口：复用本地 Kimi Chat 配置，验证 `read_image` 的 Session 内物化和
/// 聚合 User Image 投影能够完成一次正式 Host 识图闭环。
#[test]
#[ignore = "uses the configured Kimi K3 Provider and may incur a small model charge"]
fn configured_kimi_k3_reads_a_workspace_image_through_read_image() {
    run_read_image_case("kimi-k3");
}

/// 显式人工入口：复用本地 Qwen Chat 配置，验证 `read_image` 的 Session 内物化和
/// 聚合 User Image 投影能够完成一次正式 Host 识图闭环。
#[test]
#[ignore = "uses the configured Qwen Provider and may incur a small model charge"]
fn configured_qwen_reads_a_workspace_image_through_read_image() {
    run_read_image_case("qwen3.8-max");
}

fn run_read_image_case(model_key: &str) {
    let source_config = real_config_path();
    let config_text =
        fs::read_to_string(&source_config).expect("read the explicitly configured real LLM config");
    let credential = model_credential(&config_text, model_key);

    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    fs::write(runtime_home.path().join("config.toml"), config_text)
        .expect("copy config into isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated user Workspace");
    let image_path = workspace.path().join("red.png");
    solid_image(&image_path, Rgb([255, 0, 0]));

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let registered = client.runtime("register_workspace", json!({ "path": workspace.path() }));
    let workspace_id = text(&registered["workspace"]["workspace_id"]);
    let created = client.runtime(
        "create_session",
        json!({
            "title": format!("v0.17 {model_key} read_image smoke"),
            "model_key": model_key,
            "workspace_id": workspace_id
        }),
    );
    let session_id = text(&created["session"]["session_id"]);
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": format!(
                "You must call read_image exactly once with path {:?}. Then reply with exactly: RED",
                image_path
            ),
            "variant": "build",
            "idempotency_key": format!("real-{model_key}-read-image")
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(run["status"], "completed", "real image Run failed: {run}");
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| tool["tool_name"] == "read_image" && tool["status"] == "completed"),
        "real model did not complete read_image"
    );
    assert_eq!(
        run["text"].as_str().unwrap_or_default().trim(),
        "RED",
        "real model did not identify the materialized image"
    );

    let tool_image_directory = runtime_home
        .path()
        .join("data/sessions")
        .join(&session_id)
        .join("tool-images");
    let stable_images = fs::read_dir(&tool_image_directory)
        .expect("read Session tool image directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("enumerate Session tool images");
    assert_eq!(
        stable_images.len(),
        1,
        "read_image must materialize one stable copy"
    );
    assert!(
        stable_images[0]
            .file_type()
            .expect("tool image type")
            .is_file()
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
    assert!(!logs.contains(&credential));
}

fn solid_image(path: &Path, color: Rgb<u8>) {
    RgbImage::from_pixel(64, 64, color)
        .save(path)
        .expect("write fixture image");
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
        assert!(Instant::now() < deadline, "read_image Run timed out");
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

fn model_credential(document: &str, model_key: &str) -> String {
    let parsed: toml::Value =
        toml::from_str(document).unwrap_or_else(|_| panic!("real LLM config could not be parsed"));
    parsed
        .get("models")
        .and_then(|models| models.get(model_key))
        .and_then(|model| model.get("api_key"))
        .and_then(toml::Value::as_str)
        .filter(|credential| !credential.is_empty())
        .unwrap_or_else(|| panic!("{model_key} API key"))
        .to_owned()
}

fn text(value: &Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
