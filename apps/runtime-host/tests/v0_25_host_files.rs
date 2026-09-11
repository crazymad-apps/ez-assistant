//! 私有 Runtime Homes 与临时工作区验证 Host 文件读写传输，不访问已安装应用。
#![cfg(unix)]
mod support;
use reqwest::{
    StatusCode,
    blocking::{
        Client, Response,
        multipart::{Form, Part},
    },
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, time::Duration};
use support::{FakeProvider, HostProcess, write_config};

fn http() -> Client {
    Client::builder()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()
        .unwrap()
}
fn post(client: &Client, host: &HostProcess, path: &str, body: Value) -> Response {
    client
        .post(format!("{}{path}", host.base_url()))
        .bearer_auth(host.access_token())
        .json(&body)
        .send()
        .unwrap()
}

#[test]
fn unregistered_host_directories_and_previews_do_not_expand_session_roots() {
    let home = support::test_directory();
    let other_home = support::test_directory();
    let workspace = support::test_directory();
    let outside = support::test_directory();
    let provider = FakeProvider::start();
    write_config(home.path(), provider.endpoint(), "offline-file-test");
    fs::write(workspace.path().join("same.txt"), "HOST WORKSPACE").unwrap();
    fs::write(outside.path().join("same.txt"), "HOST OUTSIDE").unwrap();
    symlink(outside.path(), workspace.path().join("outside-link")).unwrap();
    let host = HostProcess::start(home.path());
    let other = HostProcess::start(other_home.path());
    let client = http();
    let route = "/host-files/list";
    assert_eq!(
        client
            .post(format!("{}{route}", host.base_url()))
            .json(&json!({"path":outside.path()}))
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{}{route}", host.base_url()))
            .bearer_auth(other.access_token())
            .json(&json!({"path":outside.path()}))
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let listing: Value = post(&client, &host, route, json!({"path":outside.path()}))
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(listing["entries"][0]["display_name"], "same.txt");
    let selected: Value = post(
        &client,
        &host,
        "/host-files/select-directory",
        json!({"path":outside.path()}),
    )
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(
        selected["path"],
        fs::canonicalize(outside.path()).unwrap().to_str().unwrap()
    );
    let preview: Value = post(
        &client,
        &host,
        "/host-files/preview",
        json!({"path":outside.path().join("same.txt")}),
    )
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(preview["text"], "HOST OUTSIDE");
    let download = post(
        &client,
        &host,
        "/host-files/download",
        json!({"path":outside.path().join("same.txt")}),
    );
    assert_eq!(download.status(), StatusCode::OK);
    assert!(
        download.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("attachment;")
    );
    assert_eq!(download.headers()["x-content-type-options"], "nosniff");
    assert_eq!(download.text().unwrap(), "HOST OUTSIDE");
    let mut runtime = host.connect();
    let registered = runtime.runtime("register_workspace", json!({"label":"M3 files", "primary_directory":workspace.path(), "additional_directories":[]}));
    let session = runtime.runtime(
        "create_session",
        json!({"workspace_id":registered["workspace"]["workspace_id"],"title":"M3 files"}),
    );
    let session_id = session["session"]["session_id"].as_str().unwrap();
    let locator =
        json!({"root":{"type":"workspace_primary"},"relative_path":"outside-link/same.txt"});
    for suffix in ["preview", "download"] {
        let response = post(
            &client,
            &host,
            &format!("/sessions/{session_id}/resource-files/{suffix}"),
            json!({"locator":locator}),
        );
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.json::<Value>().unwrap()["error"]["code"],
            "operation_not_allowed"
        );
    }
    let response = post(
        &client,
        &host,
        &format!("/sessions/{session_id}/resource-files/download"),
        json!({"locator":{"root":{"type":"workspace_primary"},"relative_path":"same.txt"}}),
    );
    assert_eq!(response.text().unwrap(), "HOST WORKSPACE");
}

#[test]
fn multipart_files_and_first_materialization_preserve_content_and_idempotency() {
    let home = support::test_directory();
    let provider = FakeProvider::start();
    write_config(home.path(), provider.endpoint(), "offline-upload-test");
    let host = HostProcess::start(home.path());
    let client = http();
    let manifest = json!({"idempotency_key":"browser-files-m3", "variant":"build", "approval_mode":"auto", "message":"Browser upload", "mode":"normal", "attachments":[{"selection_key":"web-selected","original_name":"客户端文件.txt","size_bytes":13}]});
    let upload = || {
        let form = Form::new().text("manifest", manifest.to_string()).part(
            "web-selected",
            Part::bytes(b"BROWSER BYTES".to_vec()).file_name("客户端文件.txt"),
        );
        client
            .post(format!("{}/session-materializations", host.base_url()))
            .bearer_auth(host.access_token())
            .multipart(form)
            .send()
            .unwrap()
    };
    let response = upload();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().unwrap()
    );
    let result: Value = upload().error_for_status().unwrap().json().unwrap();
    let replay: Value = upload().error_for_status().unwrap().json().unwrap();
    assert_eq!(
        result["session"]["session_id"],
        replay["session"]["session_id"]
    );
    assert_eq!(result["input_id"], replay["input_id"]);
    let session = result["session"]["session_id"].as_str().unwrap();
    let attachment = result["attachments"][0]["attachment_id"].as_str().unwrap();
    let response = client
        .get(format!(
            "{}/sessions/{session}/attachments/{attachment}/download",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .send()
        .unwrap();
    assert_eq!(response.text().unwrap(), "BROWSER BYTES");
    let form = Form::new().part(
        "file",
        Part::bytes(vec![0, 1, 2, 3]).file_name("binary.bin"),
    );
    let result: Value = client
        .post(format!(
            "{}/sessions/{session}/attachments",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .multipart(form)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let id = result["attachment"]["attachment_id"].as_str().unwrap();
    let response = client
        .get(format!(
            "{}/sessions/{session}/attachments/{id}/download",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .send()
        .unwrap();
    assert_eq!(response.bytes().unwrap().as_ref(), &[0, 1, 2, 3]);
}
