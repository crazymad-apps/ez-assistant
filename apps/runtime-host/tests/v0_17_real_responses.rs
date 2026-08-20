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

#[test]
#[ignore = "uses the configured DeepSeek Flash Provider and may incur model charges"]
fn configured_deepseek_flash_completes_a_responses_tool_loop() {
    run_deepseek_tool_case("deepseek-flash");
}

#[test]
#[ignore = "uses the configured DeepSeek Pro Provider and may incur model charges"]
fn configured_deepseek_pro_completes_a_responses_tool_loop() {
    run_deepseek_tool_case("deepseek-pro");
}

#[test]
#[ignore = "uses the configured Qwen Provider and may incur model charges"]
fn configured_qwen_reads_a_tool_image_through_responses() {
    run_read_image_case("qwen3.8-max");
}

#[test]
#[ignore = "uses the configured Kimi K3 Provider and may incur model charges"]
fn configured_kimi_reads_a_tool_image_through_responses() {
    run_read_image_case("kimi-k3");
}

fn run_deepseek_tool_case(source_model_key: &str) {
    let (runtime_home, credential) = responses_runtime_home(source_model_key);
    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    assert_responses_route(&mut client);

    let created = client.runtime(
        "create_session",
        json!({
            "title": format!("v0.17 {source_model_key} Responses smoke"),
            "model_key": "responses-real"
        }),
    );
    let session_id = text(&created["session"]["session_id"]);
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id": session_id, "approval_mode": "auto"}),
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "Call list_pinned_memories exactly once. After its result, reply with exactly: DEEPSEEK_RESPONSES_OK",
            "variant": "build",
            "idempotency_key": format!("real-{source_model_key}-responses-tool")
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(
        run["status"], "completed",
        "Responses tool Run failed: {run}"
    );
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| {
                tool["tool_name"] == "list_pinned_memories" && tool["status"] == "completed"
            }),
        "DeepSeek did not complete the Responses function call: {run}"
    );
    assert_eq!(
        run["text"].as_str().unwrap_or_default().trim(),
        "DEEPSEEK_RESPONSES_OK"
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "Reply with exactly: DEEPSEEK_REPLAY_OK",
            "variant": "build",
            "idempotency_key": format!("real-{source_model_key}-responses-replay")
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(
        run["status"], "completed",
        "reasoning replay Run failed: {run}"
    );
    assert_eq!(
        run["text"].as_str().unwrap_or_default().trim(),
        "DEEPSEEK_REPLAY_OK"
    );

    shutdown_without_secret_leak(host, client, &access_token, &credential);
}

fn run_read_image_case(source_model_key: &str) {
    let (runtime_home, credential) = responses_runtime_home(source_model_key);
    let workspace = TempDir::new().expect("isolated user Workspace");
    let image_path = workspace.path().join("red.png");
    solid_image(&image_path, Rgb([255, 0, 0]));

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    assert_responses_route(&mut client);
    let registered = client.runtime("register_workspace", json!({"path": workspace.path()}));
    let workspace_id = text(&registered["workspace"]["workspace_id"]);
    let created = client.runtime(
        "create_session",
        json!({
            "title": format!("v0.17 {source_model_key} Responses image smoke"),
            "model_key": "responses-real",
            "workspace_id": workspace_id
        }),
    );
    let session_id = text(&created["session"]["session_id"]);
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id": session_id, "approval_mode": "auto"}),
    );

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": format!(
                "Call read_image exactly once with path {:?}. Inspect its returned image, then reply with exactly: RED",
                image_path
            ),
            "variant": "build",
            "idempotency_key": format!("real-{source_model_key}-responses-read-image")
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(
        run["status"], "completed",
        "Responses image Run failed: {run}"
    );
    assert!(
        run["tools"]
            .as_array()
            .expect("tool activities")
            .iter()
            .any(|tool| tool["tool_name"] == "read_image" && tool["status"] == "completed"),
        "model did not complete read_image: {run}"
    );
    assert_eq!(run["text"].as_str().unwrap_or_default().trim(), "RED");

    let tool_images = runtime_home
        .path()
        .join("data/sessions")
        .join(&session_id)
        .join("tool-images");
    assert_eq!(
        fs::read_dir(tool_images)
            .expect("read tool image directory")
            .count(),
        1,
        "one read_image call must create one stable Session copy"
    );

    shutdown_without_secret_leak(host, client, &access_token, &credential);
}

fn responses_runtime_home(source_model_key: &str) -> (TempDir, String) {
    let source_config = real_config_path();
    let document = fs::read_to_string(source_config).expect("read configured real LLM config");
    let mut parsed: toml::Value = toml::from_str(&document).expect("parse real LLM config");
    let root = parsed.as_table_mut().expect("config root table");
    let models = root
        .get_mut("models")
        .and_then(toml::Value::as_table_mut)
        .expect("models table");
    let mut route = models
        .get(source_model_key)
        .unwrap_or_else(|| panic!("configured model `{source_model_key}`"))
        .clone();
    let route_table = route.as_table_mut().expect("model route table");
    let credential = route_table
        .get("api_key")
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{source_model_key} API key"))
        .to_owned();
    route_table.insert(
        "protocol".to_owned(),
        toml::Value::String("openai_responses".to_owned()),
    );
    models.insert("responses-real".to_owned(), route);
    root.insert(
        "default_model".to_owned(),
        toml::Value::String("responses-real".to_owned()),
    );

    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    fs::write(
        runtime_home.path().join("config.toml"),
        toml::to_string(&parsed).expect("serialize isolated Responses config"),
    )
    .expect("write isolated Responses config");
    (runtime_home, credential)
}

fn assert_responses_route(client: &mut support::Client) {
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let models = client.runtime("list_models", json!({}));
    assert!(
        models["models"]
            .as_array()
            .expect("configured models")
            .iter()
            .any(|model| {
                model["model_key"] == "responses-real" && model["protocol"] == "openai_responses"
            }),
        "isolated route did not compile as Responses: {models}"
    );
}

fn wait_for_terminal(client: &mut support::Client, session_id: &str, run_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let run = client.runtime(
            "get_run",
            json!({"session_id": session_id, "run_id": run_id}),
        )["run"]
            .clone();
        if matches!(
            run["status"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "compaction_required")
        ) {
            return run;
        }
        assert!(Instant::now() < deadline, "Responses real Run timed out");
        thread::sleep(Duration::from_millis(100));
    }
}

fn shutdown_without_secret_leak(
    host: HostProcess,
    mut client: support::Client,
    access_token: &str,
    credential: &str,
) {
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    let output = host.wait();
    assert!(output.status.success());
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!logs.contains(access_token));
    assert!(!logs.contains(credential));
}

fn solid_image(path: &Path, color: Rgb<u8>) {
    RgbImage::from_pixel(64, 64, color)
        .save(path)
        .expect("write fixture image");
}

fn real_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("EZ_ASSISTANT_REAL_LLM_CONFIG") {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .expect("home directory; or set EZ_ASSISTANT_REAL_LLM_CONFIG")
        .join(".ez-assistant/config.toml")
}

fn text(value: &Value) -> String {
    value.as_str().expect("JSON string").to_owned()
}
