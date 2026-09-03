//! 正式 Host 的 MCP 调用验收；仅本地 fixture 和 TempDir，不引入产品专用测试模式。

mod events;
mod faults;
mod fixture;

use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{Client, HostProcess};
use events::{EventCapture, assert_invocation_order};
use fixture::{MODEL_SECRET, SECRET, WireFixture};

#[test]
fn formal_host_mcp_roundtrips_chat_and_responses_over_both_transports() {
    for protocol in ["openai_chat_completions", "openai_responses"] {
        for transport in ["stdio", "http"] {
            for selected in [false, true] {
                roundtrip(protocol, transport, selected, "allow_once");
            }
        }
    }
}

#[test]
fn formal_host_denied_mcp_never_reaches_authenticated_remote() {
    for transport in ["stdio", "http"] {
        roundtrip("openai_responses", transport, false, "deny");
    }
}

#[test]
fn formal_host_mcp_auth_failure_is_redacted_and_draft_is_not_saved() {
    let home = TempDir::new().expect("isolated home");
    let fixture = WireFixture::start("target", false, String::new());
    configure(home.path(), &fixture, "openai_chat_completions", "http");
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let before = client.runtime("get_mcp_configuration", json!({}));
    let result = client.runtime("test_mcp_server", json!({"test_id":"bad-auth","server":{
        "server_key":"candidate","display_name":"Candidate","description":"Offline auth rejection","enabled":true,
        "transport":{"type":"streamable_http","payload":{"url":{"mode":"replace","value":format!("{}/mcp",fixture.server.endpoint())},"headers":{"Authorization":{"mode":"replace","value":"wrong"}}}}
    }}));
    assert_eq!(result["outcome"], "failure");
    assert_clean(result.to_string().as_bytes());
    assert_eq!(client.runtime("get_mcp_configuration", json!({})), before);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    let output = host.wait();
    assert!(output.status.success());
    assert_clean(&output.stdout);
    assert_clean(&output.stderr);
    assert!(
        fixture
            .state
            .requests
            .lock()
            .expect("model requests")
            .is_empty()
    );
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
}

fn roundtrip(protocol: &str, transport: &str, selected: bool, decision: &str) {
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    let mut png = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(8, 8)
        .write_to(&mut png, ImageFormat::Png)
        .expect("fixture PNG");
    let fixture = WireFixture::start("target", selected, STANDARD.encode(png.into_inner()));
    configure(runtime_home.path(), &fixture, protocol, transport);
    let host = HostProcess::start(runtime_home.path());
    let events = EventCapture::start(&host);
    let mut client = host.connect();
    let config = client.runtime("get_mcp_configuration", json!({}));
    assert_clean(config.to_string().as_bytes());
    assert_eq!(
        config["snapshot"]["servers"][0]["runtime_state"],
        "connected"
    );
    let session = client.runtime(
        "create_session",
        json!({"title":"M8 MCP wire", "model_key":"fixture"}),
    );
    let session_id = session["session"]["session_id"]
        .as_str()
        .expect("session id");
    let mut input = json!({"session_id":session_id,"message":"Read a fixture value","variant":"plan","idempotency_key":"m8-input"});
    if selected {
        input["mcp_server_key"] = json!("target");
    }
    let submitted = client.runtime("submit_input", input);
    let run_id = submitted["run"]["run_id"].as_str().expect("run id");
    let approval = wait_approval(&mut client, session_id);
    assert_eq!(approval["subject"]["type"], "mcp");
    let subject = &approval["subject"];
    assert_eq!(subject["identity"]["server_key"], "target");
    assert_eq!(subject["identity"]["tool_name"], "first_tool");
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    assert!(!runtime_home.path().join("fixture-calls").exists());
    assert_clean(approval.to_string().as_bytes());
    client.runtime(
        "decide_approval",
        json!({"session_id":session_id,"approval_id":approval["approval_id"],"decision":decision}),
    );
    client.wait_for_status("MCP wire roundtrip", session_id, run_id, &["completed"]);
    let conversation = client.conversation(session_id);
    assert!(conversation.to_string().contains("MCP fixture completed"));
    assert_clean(conversation.to_string().as_bytes());
    let requests = fixture
        .state
        .requests
        .lock()
        .expect("model requests")
        .clone();
    assert_eq!(requests.len(), if selected { 2 } else { 3 });
    let first = requests[0].to_string();
    assert!(first.contains(if selected {
        "MCP_SERVER_SELECTION_V1"
    } else {
        "MCP_SERVER_DIRECTORY_V1"
    }));
    assert_eq!(
        first.contains("first_tool"),
        selected,
        "only selected tools are initially disclosed"
    );
    for request in &requests {
        assert_clean(request.to_string().as_bytes());
        let tools = request["tools"].as_array().expect("native tools");
        let names = tools
            .iter()
            .map(|tool| {
                if protocol == "openai_responses" {
                    &tool["name"]
                } else {
                    &tool["function"]["name"]
                }
            })
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| **name == "call_mcp_tool"));
        assert!(names.iter().any(|name| **name == "discover_mcp_tools"));
        assert!(!names.iter().any(|name| **name == "first_tool"));
    }
    let last = requests.last().expect("final request").to_string();
    if decision == "allow_once" {
        assert!(
            last.contains("called:first_tool"),
            "real MCP result must reach the next model request"
        );
        assert!(
            last.contains("data:image/"),
            "MCP image must reach the selected model protocol"
        );
        assert_image_preview(&host, &mut client, session_id, &conversation);
    } else {
        assert!(!last.contains("called:first_tool"));
        assert!(!last.contains("data:image/"));
    }
    if transport == "http" {
        assert_eq!(
            fixture.state.calls.load(Ordering::SeqCst),
            usize::from(decision == "allow_once")
        );
        let methods = fixture.state.methods.lock().expect("method capture");
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "initialize")
                .count(),
            1
        );
    }
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    let output = host.wait();
    assert!(output.status.success());
    assert_clean(&output.stdout);
    assert_clean(&output.stderr);
    let captured = events.finish();
    assert_clean(&captured);
    assert_invocation_order(&captured, decision == "allow_once");
    if transport == "stdio" {
        let calls =
            fs::read_to_string(runtime_home.path().join("fixture-calls")).unwrap_or_default();
        assert_eq!(
            calls.lines().count(),
            usize::from(decision == "allow_once"),
            "stdio must not replay a call"
        );
    }
    scan_data(&runtime_home.path().join("data"));
    let connection = Connection::open_with_flags(
        runtime_home.path().join("data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read isolated database");
    let counts: (i64, i64, i64) = connection.query_row("SELECT (SELECT COUNT(*) FROM inputs), (SELECT COUNT(*) FROM runs), (SELECT COUNT(*) FROM pending_tool_exchanges)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).expect("exact completed counts");
    assert_eq!(counts, (1, 1, 0));
    eprintln!("MCP wire verified: {protocol}, {transport}, selected={selected}, {decision}");
}

fn configure(home: &Path, fixture: &WireFixture, protocol: &str, transport: &str) {
    let projection = if protocol == "openai_responses" {
        "native_function_output"
    } else {
        "aggregated_user_input"
    };
    fs::write(
        home.join("config.toml"),
        format!(
            r#"schema_version = 1
default_model = "fixture"
[models.fixture]
protocol = "{protocol}"
provider = "fixture"
endpoint = "{}"
model = "offline-mcp"
api_key = "{MODEL_SECRET}"
context_window_tokens = 32768
max_output_tokens = 4096
[models.fixture.capabilities]
tool_calls = true
streaming = true
image_input = true
tool_image_projection = "{projection}"
"#,
            fixture.server.endpoint()
        ),
    )
    .expect("write isolated config");
    let mut server = if transport == "http" {
        json!({"url":format!("{}/mcp",fixture.server.endpoint()),"headers":{"Authorization":SECRET}})
    } else {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
        json!({"command":"python3","args":["-u",script],"env":{"TOKEN":SECRET,"MCP_FIXTURE_IMAGE":fixture.state.image,"MCP_FIXTURE_AUDIT":home.join("fixture-calls"),"MCP_FIXTURE_STDERR_FLOOD":"1"}})
    };
    let mode = fixture.state.behavior.as_str();
    if mode != "reply" {
        server["toolTimeoutMs"] = json!(1000);
        if transport == "stdio" {
            server["env"]["MCP_FIXTURE_CALL_MODE"] = json!(mode);
        }
    }
    fs::write(
        home.join("mcp.json"),
        json!({"mcpServers":{"target":server}}).to_string(),
    )
    .expect("write isolated MCP config");
}

fn wait_approval(client: &mut Client, session_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let pending = client.runtime("list_pending_approvals", json!({"session_id":session_id}));
        if let Some(approval) = pending["approvals"].as_array().expect("approvals").first() {
            return approval.clone();
        }
        assert!(Instant::now() < deadline, "MCP approval was not requested");
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_image_preview(
    host: &HostProcess,
    client: &mut Client,
    session_id: &str,
    conversation: &Value,
) {
    let message = conversation["items"]
        .as_array()
        .expect("conversation")
        .iter()
        .find(|item| item.to_string().contains("m8-invoke") && item["type"] == "assistant")
        .expect("MCP call message");
    let message_id = message["message_id"].as_str().expect("message id");
    let detail = client.runtime("get_tool_detail", json!({"owner":{"type":"main_session","session_id":session_id},"message_id":message_id,"call_id":"m8-invoke"}))["snapshot"]["value"].clone();
    assert_clean(detail.to_string().as_bytes());
    let file = detail["files"]
        .as_array()
        .expect("tool files")
        .iter()
        .find(|file| file["origin"] == "session_tool_image")
        .expect("MCP image resource");
    let resource = file["resource_ref_id"].as_str().expect("resource id");
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("preview client")
        .get(format!(
            "{}/sessions/{session_id}/messages/{message_id}/resources/{resource}/preview",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .send()
        .expect("MCP preview");
    assert!(response.status().is_success());
    assert!(image::load_from_memory(&response.bytes().expect("preview bytes")).is_ok());
}

fn assert_clean(bytes: &[u8]) {
    for secret in [SECRET, MODEL_SECRET] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "credential marker leaked across a public or persisted boundary"
        );
    }
}

fn scan_data(path: &Path) {
    for entry in fs::read_dir(path).expect("isolated data directory") {
        let entry = entry.expect("data entry");
        let kind = entry.file_type().expect("entry type");
        if kind.is_dir() {
            scan_data(&entry.path());
        } else if kind.is_file() {
            assert_clean(&fs::read(entry.path()).expect("read isolated data"));
        }
    }
}
