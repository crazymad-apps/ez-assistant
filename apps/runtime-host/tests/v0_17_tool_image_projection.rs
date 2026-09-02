#![cfg(unix)]

mod support;

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    thread,
    time::{Duration, Instant},
};

use image::{GenericImageView as _, Rgb, RgbImage};
use reqwest::blocking::Client as HttpClient;
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_qwen_image_config};

#[test]
fn formal_host_projects_main_and_child_tool_images_across_restart_and_failures() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated Workspace");
    let image_path = workspace.path().join("fixture.png");
    RgbImage::from_pixel(640, 320, Rgb([25, 50, 75]))
        .save(&image_path)
        .expect("write source image");
    write_qwen_image_config(
        runtime_home.path(),
        provider.endpoint(),
        "offline-image-secret",
    );

    let first_host = HostProcess::start(runtime_home.path());
    let mut first = first_host.connect();
    let workspace_id = text(
        &first.runtime(
            "register_workspace",
            json!({
                "label": "tool-image-workspace",
                "primary_directory": workspace.path(),
                "additional_directories": [],
            }),
        )["workspace"]["workspace_id"],
    );
    let session_id = text(
        &first.runtime(
            "create_session",
            json!({
                "title": "Tool image projection",
                "model_key": "qwen-image-fixture",
                "workspace_id": workspace_id,
            }),
        )["session"]["session_id"],
    );
    first.runtime(
        "set_session_approval_mode",
        json!({"session_id": session_id, "approval_mode": "auto"}),
    );

    let main_run = submit_and_wait(
        &mut first,
        &session_id,
        &format!("READ_IMAGE_CASE {}", image_path.display()),
        "main-image",
    );
    assert_eq!(main_run["status"], "completed");
    let main_page = first.conversation(&session_id);
    let (main_message_id, main_call_id) = image_tool_identity(&main_page);
    let main_detail = tool_detail(
        &mut first,
        json!({"type":"main_session", "session_id":session_id}),
        &main_message_id,
        &main_call_id,
    );
    let main_resource_id = image_resource_id(&main_detail);

    let child_parent = first.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":format!("DELEGATE_IMAGE_CASE {}", image_path.display()),
            "variant":"build",
            "idempotency_key":"child-image",
        }),
    );
    let child_parent_run_id = text(&child_parent["run"]["run_id"]);
    let child_deadline = Instant::now() + Duration::from_secs(20);
    let child = loop {
        let tasks = first.runtime(
            "list_child_tasks",
            json!({"session_id":session_id, "parent_run_id":child_parent_run_id}),
        )["tasks"]
            .as_array()
            .expect("child tasks")
            .clone();
        if let Some(child) = tasks.first()
            && child["status"] == "completed"
        {
            break child.clone();
        }
        assert!(
            Instant::now() < child_deadline,
            "image child timed out: {tasks:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    first.runtime(
        "interrupt_run",
        json!({"session_id":session_id, "run_id":child_parent_run_id}),
    );
    let _ = first.wait_for_status(
        "cancel parent after child projection",
        &session_id,
        &child_parent_run_id,
        &["cancelled", "completed"],
    );
    let child_task_id = text(&child["child_task_id"]);
    let child_page = first.runtime(
        "get_child_task_view",
        json!({"session_id":session_id, "child_task_id":child_task_id}),
    )["snapshot"]["value"]["conversation"]
        .clone();
    let (child_message_id, child_call_id) = image_tool_identity(&child_page);
    let child_owner = json!({
        "type":"child_task",
        "session_id":session_id,
        "child_task_id":child_task_id,
    });
    let child_detail = tool_detail(
        &mut first,
        child_owner.clone(),
        &child_message_id,
        &child_call_id,
    );
    let child_resource_id = image_resource_id(&child_detail);

    let view = first.runtime("get_session_view", json!({"session_id":session_id}));
    assert!(
        view["snapshot"]["value"]["file_references"]
            .as_array()
            .expect("file references")
            .is_empty()
    );
    assert!(
        view["snapshot"]["value"]["attachments"]
            .as_array()
            .expect("attachments")
            .is_empty()
    );

    assert_preview(
        &first_host,
        &session_id,
        None,
        &main_message_id,
        &main_resource_id,
    );
    assert_preview(
        &first_host,
        &session_id,
        Some(&child_task_id),
        &child_message_id,
        &child_resource_id,
    );
    let tool_image_directory = runtime_home
        .path()
        .join("data/sessions")
        .join(&session_id)
        .join("tool-images");
    let entries = fs::read_dir(&tool_image_directory)
        .expect("tool image directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("tool image entries");
    assert_eq!(entries.len(), 1, "preview must not persist a thumbnail");
    let stored_image = entries[0].path();

    first.runtime("shutdown_runtime", json!({}));
    drop(first);
    assert!(first_host.wait().status.success());

    let second_host = HostProcess::start(runtime_home.path());
    let mut second = second_host.connect();
    assert_eq!(
        image_resource_id(&tool_detail(
            &mut second,
            json!({"type":"main_session", "session_id":session_id}),
            &main_message_id,
            &main_call_id,
        )),
        main_resource_id
    );
    assert_eq!(
        image_resource_id(&tool_detail(
            &mut second,
            child_owner,
            &child_message_id,
            &child_call_id,
        )),
        child_resource_id
    );
    assert_preview(
        &second_host,
        &session_id,
        Some(&child_task_id),
        &child_message_id,
        &child_resource_id,
    );

    let missing = tool_image_directory.join("temporarily-missing");
    fs::rename(&stored_image, &missing).expect("hide stable image");
    assert_preview_unavailable(
        &second_host,
        &session_id,
        None,
        &main_message_id,
        &main_resource_id,
    );
    assert!(
        !stored_image.exists(),
        "preview must not repair from the external source"
    );
    fs::rename(&missing, &stored_image).expect("restore stable image");

    fs::set_permissions(&stored_image, fs::Permissions::from_mode(0o600))
        .expect("make corruption fixture writable");
    fs::write(&stored_image, b"corrupt").expect("corrupt stable image");
    assert_preview_unavailable(
        &second_host,
        &session_id,
        Some(&child_task_id),
        &child_message_id,
        &child_resource_id,
    );

    second.runtime("shutdown_runtime", json!({}));
    drop(second);
    assert!(second_host.wait().status.success());
}

fn submit_and_wait(
    client: &mut support::Client,
    session_id: &str,
    message: &str,
    idempotency_key: &str,
) -> Value {
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":message,
            "variant":"build",
            "idempotency_key":idempotency_key,
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run =
            client.runtime("get_run", json!({"session_id":session_id, "run_id":run_id}))["run"]
                .clone();
        if run["status"] == "completed" {
            return run;
        }
        assert!(
            !matches!(
                run["status"].as_str(),
                Some("failed" | "cancelled" | "interrupted")
            ),
            "{message}: unexpected terminal Run: {run}"
        );
        assert!(Instant::now() < deadline, "{message}: timed out: {run}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn tool_detail(
    client: &mut support::Client,
    owner: Value,
    message_id: &str,
    call_id: &str,
) -> Value {
    client.runtime(
        "get_tool_detail",
        json!({"owner":owner, "message_id":message_id, "call_id":call_id}),
    )["snapshot"]["value"]
        .clone()
}

fn image_tool_identity(conversation: &Value) -> (String, String) {
    for item in conversation["items"]
        .as_array()
        .expect("conversation items")
    {
        let Some(segments) = item.get("segments").and_then(Value::as_array) else {
            continue;
        };
        for tool in segments
            .iter()
            .filter(|segment| segment["type"] == "tool_group")
            .flat_map(|segment| segment["tools"].as_array().expect("tool group"))
        {
            if tool["tool_name"] == "read_image" {
                return (text(&item["message_id"]), text(&tool["call_id"]));
            }
        }
    }
    panic!("read_image tool event not found: {conversation}");
}

fn image_resource_id(detail: &Value) -> String {
    let files = detail["files"].as_array().expect("tool detail files");
    assert_eq!(files.len(), 1, "read_image exposes only its copied image");
    assert_eq!(files[0]["origin"], "session_tool_image");
    assert!(files[0]["display_path"].is_null());
    text(&files[0]["resource_ref_id"])
}

fn assert_preview(
    host: &HostProcess,
    session_id: &str,
    child_task_id: Option<&str>,
    message_id: &str,
    resource_id: &str,
) {
    let response = preview_request(host, session_id, child_task_id, message_id, resource_id);
    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .expect("content type"),
        "image/png"
    );
    let image =
        image::load_from_memory(&response.bytes().expect("preview bytes")).expect("decode preview");
    assert_eq!(image.dimensions(), (640, 320));
}

fn assert_preview_unavailable(
    host: &HostProcess,
    session_id: &str,
    child_task_id: Option<&str>,
    message_id: &str,
    resource_id: &str,
) {
    let response = preview_request(host, session_id, child_task_id, message_id, resource_id);
    assert!(!response.status().is_success());
    let body: Value = response.json().expect("resource error body");
    assert_eq!(body["error"]["code"], "attachment_unavailable");
}

fn preview_request(
    host: &HostProcess,
    session_id: &str,
    child_task_id: Option<&str>,
    message_id: &str,
    resource_id: &str,
) -> reqwest::blocking::Response {
    let path = child_task_id.map_or_else(
        || format!("sessions/{session_id}/messages/{message_id}/resources/{resource_id}/preview"),
        |child_task_id| {
            format!("sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_id}/preview")
        },
    );
    HttpClient::new()
        .get(format!("{}/{path}", host.base_url()))
        .bearer_auth(host.access_token())
        .send()
        .expect("preview request")
}

fn text(value: &Value) -> String {
    value.as_str().expect("string value").to_owned()
}
