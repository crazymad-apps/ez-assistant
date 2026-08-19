#![cfg(unix)]

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use image::{Rgb, RgbImage};
use reqwest::blocking::{Client as HttpClient, multipart};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::HostProcess;

/// 显式人工入口：复用本地 `kimi-k3` 配置，在隔离 Runtime Home 中验证两张图片经过
/// 正式上传、公共预处理、OpenAI-compatible 编码和 Kimi Code Endpoint 后能够完成原生识图。
#[test]
#[ignore = "uses the configured Kimi K3 Provider and may incur a small model charge"]
fn configured_kimi_k3_reads_two_images_through_the_formal_host() {
    run_two_image_case("kimi-k3", true, &["kimi-k3"]);
}

/// 显式人工入口：复用本地 `qwen3.8-max` 配置，验证国内 OpenAI-compatible
/// 服务的原生多模态请求与 reasoning 响应链路。
#[test]
#[ignore = "uses the configured Qwen Provider and may incur a small model charge"]
fn configured_qwen_reads_two_images_through_the_formal_host() {
    run_two_image_case("qwen3.8-max", true, &["qwen3.8-max"]);
}

/// 显式人工入口：以无原生视觉的 `deepseek-flash` 为主模型，验证它通过配置的
/// `qwen3.8-max` 辅助视觉模型和 `inspect_images` 完成同一图片集识别。
#[test]
#[ignore = "uses configured DeepSeek and Qwen Providers and may incur small model charges"]
fn configured_deepseek_uses_the_auxiliary_vision_model() {
    run_two_image_case("deepseek-flash", false, &["deepseek-flash", "qwen3.8-max"]);
}

fn run_two_image_case(
    model_key: &str,
    expected_native_image_input: bool,
    credential_models: &[&str],
) {
    let source_config = real_config_path();
    let config_text =
        fs::read_to_string(&source_config).expect("read the explicitly configured real LLM config");
    let credentials = credential_models
        .iter()
        .map(|key| model_credential(&config_text, key))
        .collect::<Vec<_>>();

    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    fs::write(runtime_home.path().join("config.toml"), config_text)
        .expect("copy config into isolated Runtime Home");
    let images = TempDir::new().expect("isolated image directory");
    let red = images.path().join("red.png");
    let blue = images.path().join("blue.png");
    solid_image(&red, Rgb([255, 0, 0]));
    solid_image(&blue, Rgb([0, 0, 255]));

    let host = HostProcess::start(runtime_home.path());
    let access_token = host.access_token().to_owned();
    let mut client = host.connect();
    let configuration = client.runtime("get_config_status", json!({}));
    assert_eq!(configuration["status"]["state"], "ready");
    let models = client.runtime("list_models", json!({}));
    assert!(
        models["models"]
            .as_array()
            .expect("configured models")
            .iter()
            .any(|model| {
                model["model_key"] == model_key
                    && model["supports_image_input"] == expected_native_image_input
            }),
        "model image capability does not match the expected route"
    );
    if !expected_native_image_input {
        assert_eq!(
            configuration["status"]["auxiliary_vision_model"], "qwen3.8-max",
            "auxiliary vision model is not configured"
        );
    }

    let created = client.runtime(
        "create_session",
        json!({
            "title": format!("v0.16 {model_key} multimodal smoke"),
            "model_key": model_key
        }),
    );
    let session_id = text(&created["session"]["session_id"]);
    if !expected_native_image_input {
        client.runtime(
            "set_session_approval_mode",
            json!({ "session_id": session_id, "approval_mode": "auto" }),
        );
    }
    let red_id = upload(&host, &session_id, &red);
    let blue_id = upload(&host, &session_id, &blue);
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": if expected_native_image_input {
                "Inspect the two attached solid-color images directly without calling any tools. Reply with exactly: RED BLUE"
            } else {
                "Use inspect_images to inspect the two attached solid-color images in attachment order. Then reply with exactly: RED BLUE"
            },
            "variant": "build",
            "attachment_ids": [red_id, blue_id],
            "idempotency_key": format!("real-{model_key}-two-images")
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let run = wait_for_terminal(&mut client, &session_id, &run_id);
    assert_eq!(run["status"], "completed", "real image Run failed: {run}");
    let answer = run["text"].as_str().unwrap_or_default().to_uppercase();
    assert!(
        answer.contains("RED") && answer.contains("BLUE"),
        "model route did not identify both images"
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
    for credential in credentials {
        assert!(!logs.contains(&credential));
    }
}

fn solid_image(path: &Path, color: Rgb<u8>) {
    RgbImage::from_pixel(64, 64, color)
        .save(path)
        .expect("write fixture image");
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
                .expect("build image upload part"),
        )
        .send()
        .expect("upload image attachment");
    let status = response.status();
    let body: Value = response.json().expect("image upload JSON");
    assert!(status.is_success(), "image upload failed");
    text(&body["attachment"]["attachment_id"])
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
        assert!(Instant::now() < deadline, "multimodal Run timed out");
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
