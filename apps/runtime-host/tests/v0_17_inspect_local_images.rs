#![cfg(unix)]

mod support;

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use image::{Rgb, RgbImage};
use serde_json::json;
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_auxiliary_vision_config};

#[test]
fn auxiliary_inspector_reads_relative_and_absolute_local_images_without_artifacts() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let workspace = TempDir::new().expect("isolated Workspace");
    let outside = TempDir::new().expect("isolated outside directory");
    let relative_image = workspace.path().join("relative.png");
    let absolute_image = outside.path().join("absolute.png");
    RgbImage::from_pixel(32, 24, Rgb([255, 0, 0]))
        .save(&relative_image)
        .expect("write relative image");
    RgbImage::from_pixel(24, 32, Rgb([0, 0, 255]))
        .save(&absolute_image)
        .expect("write absolute image");
    write_auxiliary_vision_config(
        runtime_home.path(),
        provider.endpoint(),
        "offline-vision-secret",
    );

    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let workspace_id = text(
        &client.runtime("register_workspace", json!({"path":workspace.path()}))["workspace"]["workspace_id"],
    );
    let session_id = text(
        &client.runtime(
            "create_session",
            json!({
                "title":"Inspect local images",
                "model_key":"text-fixture",
                "workspace_id":workspace_id,
            }),
        )["session"]["session_id"],
    );
    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id":session_id,
            "message":format!(
                "INSPECT_LOCAL_CASE relative.png|{}",
                absolute_image.display()
            ),
            "variant":"build",
            "idempotency_key":"inspect-local-images",
        }),
    );
    let run_id = text(&submitted["run"]["run_id"]);
    let approval = wait_for_approval(&mut client, &session_id);
    let resolved_relative_image =
        fs::canonicalize(&relative_image).expect("canonical relative image");
    assert_eq!(approval["subject"]["type"], "files");
    assert_eq!(approval["subject"]["tool_name"], "inspect_images");
    assert_eq!(approval["subject"]["operation"], "read");
    assert_eq!(
        approval["subject"]["paths"],
        json!([resolved_relative_image, absolute_image])
    );
    assert_eq!(
        approval["available_decisions"],
        json!(["allow_once", "allow_session", "allow_workspace", "deny"])
    );
    client.runtime(
        "decide_approval",
        json!({
            "session_id":session_id,
            "approval_id":approval["approval_id"],
            "decision":"allow_session"
        }),
    );
    let run = client.wait_for_status("inspect local images", &session_id, &run_id, &["completed"]);
    assert_eq!(run["status"], "completed");

    let conversation = client.conversation(&session_id);
    let conversation_text = conversation.to_string();
    assert!(conversation_text.contains("inspect_images"));
    assert!(conversation_text.contains("LOCAL IMAGES VERIFIED"));
    assert!(conversation_text.contains("local inspect answer"));
    let view = client.runtime("get_session_view", json!({"session_id":session_id}));
    assert!(
        view["snapshot"]["value"]["attachments"]
            .as_array()
            .expect("attachments")
            .is_empty()
    );
    assert!(
        view["snapshot"]["value"]["file_references"]
            .as_array()
            .expect("file references")
            .is_empty()
    );
    let tool_image_directory = runtime_home
        .path()
        .join("data/sessions")
        .join(&session_id)
        .join("tool-images");
    assert_eq!(
        fs::read_dir(tool_image_directory)
            .expect("tool image directory")
            .count(),
        0,
        "inspect_images must not create persisted image artifacts"
    );
    let permissions: serde_json::Value = serde_json::from_slice(
        &fs::read(
            runtime_home
                .path()
                .join("data/sessions")
                .join(&session_id)
                .join("private/permissions.json"),
        )
        .expect("read session permissions"),
    )
    .expect("parse session permissions");
    for path in [&resolved_relative_image, &absolute_image] {
        assert!(
            permissions["rules"]
                .as_array()
                .expect("permission rules")
                .iter()
                .any(|rule| {
                    rule["effect"] == "allow"
                        && rule["matcher"]["type"] == "file"
                        && rule["matcher"]["operation"] == "read"
                        && rule["matcher"]["path"] == path.to_string_lossy().as_ref()
                        && rule["matcher"]["path_match"] == "exact"
                })
        );
    }

    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

fn wait_for_approval(client: &mut support::Client, session_id: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let approvals = client.runtime("list_pending_approvals", json!({"session_id":session_id}))
            ["approvals"]
            .as_array()
            .expect("pending approvals")
            .clone();
        if let Some(approval) = approvals.into_iter().next() {
            return approval;
        }
        assert!(
            Instant::now() < deadline,
            "inspect_images approval timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn text(value: &serde_json::Value) -> String {
    value.as_str().expect("string value").to_owned()
}
