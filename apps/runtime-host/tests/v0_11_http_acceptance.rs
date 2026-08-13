#![cfg(all(unix, feature = "web-demo"))]

mod support;

use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

use reqwest::{
    blocking::Client as HttpClient,
    header::{
        ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD,
        HOST, ORIGIN,
    },
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use tempfile::TempDir;

use support::{FakeProvider, HostProcess, write_config};

const TEST_API_KEY: &str = "http-acceptance-secret-must-not-leak";

#[test]
fn local_http_enforces_access_boundaries_streams_events_and_serves_opt_in_demo() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(runtime_home.path(), provider.endpoint(), TEST_API_KEY);

    let disabled_host = HostProcess::start(runtime_home.path());
    let http = HttpClient::new();
    assert_eq!(
        http.get(format!("{}/demo/", disabled_host.base_url()))
            .bearer_auth(disabled_host.access_token())
            .send()
            .expect("disabled Demo request")
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let mut disabled_client = disabled_host.connect();
    disabled_client.runtime("shutdown_runtime", json!({}));
    let disabled_output = disabled_host.wait();
    assert!(disabled_output.status.success());
    assert!(!runtime_home.path().join("run/runtime.json").exists());

    let host = HostProcess::start_web_demo(runtime_home.path());
    let base_url = host.base_url().to_owned();
    let token = host.access_token().to_owned();

    let unauthorized = http
        .get(format!("{base_url}/health"))
        .send()
        .expect("unauthorized health");
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    let browser_unauthorized = http
        .get(format!("{base_url}/health"))
        .header(ORIGIN, "http://localhost:1420")
        .send()
        .expect("browser unauthorized health");
    assert_eq!(
        browser_unauthorized.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:1420"
    );

    let wrong_host = http
        .get(format!("{base_url}/health"))
        .bearer_auth(&token)
        .header(HOST, "localhost:1")
        .send()
        .expect("wrong Host");
    assert_eq!(wrong_host.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_origin = http
        .get(format!("{base_url}/health"))
        .bearer_auth(&token)
        .header(ORIGIN, "https://untrusted.example")
        .send()
        .expect("wrong Origin");
    assert_eq!(wrong_origin.status(), reqwest::StatusCode::FORBIDDEN);

    let preflight = http
        .request(reqwest::Method::OPTIONS, format!("{base_url}/commands"))
        .header(ORIGIN, "http://localhost:1420")
        .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization, content-type",
        )
        .send()
        .expect("CORS preflight");
    assert_eq!(preflight.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        preflight.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:1420"
    );

    let capabilities: serde_json::Value = http
        .get(format!("{base_url}/capabilities"))
        .bearer_auth(&token)
        .send()
        .expect("capabilities")
        .json()
        .expect("capabilities JSON");
    assert_eq!(capabilities["protocol_version"], 1);
    assert_eq!(capabilities["sse"], true);
    assert_eq!(capabilities["streaming_upload"], true);
    assert_eq!(capabilities["private_web_demo"], true);
    assert_eq!(capabilities["max_attachment_bytes"], 1024_u64 * 1024 * 1024);

    let oversized = http
        .post(format!("{base_url}/commands"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(vec![b'x'; 1024 * 1024 + 1])
        .send()
        .expect("oversized command");
    assert_eq!(oversized.status(), reqwest::StatusCode::BAD_REQUEST);
    let oversized_body: serde_json::Value = oversized.json().expect("oversized JSON error");
    assert_eq!(oversized_body["error"]["code"], "invalid_request");

    let demo = http
        .get(format!("{base_url}/demo/"))
        .send()
        .expect("Web Demo");
    assert_eq!(demo.status(), reqwest::StatusCode::OK);
    let demo_html = demo.text().expect("Demo HTML");
    assert!(demo_html.contains("EZ Assistant Runtime"));
    assert!(demo_html.contains("message-list"));
    assert!(demo_html.contains("workspace-list"));
    assert!(demo_html.contains("attachment-files"));
    assert!(demo_html.contains("archive-session"));
    assert!(demo_html.contains("usage-input"));
    assert!(demo_html.contains("usage-output"));
    assert!(demo_html.contains("usage-total"));
    assert!(demo_html.contains("usage-cached"));
    assert!(demo_html.contains("agent-variant"));
    assert!(demo_html.contains("approval-mode"));
    assert!(demo_html.contains("reload-permissions"));
    assert!(demo_html.contains("approval-list"));
    assert!(demo_html.contains("quick-plan"));
    assert!(demo_html.contains("quick-build"));
    assert!(demo_html.contains("child-task-list"));
    assert!(demo_html.contains("usage-combined-total"));
    assert!(demo_html.contains("/demo/child-tasks.js"));
    assert!(!demo_html.contains("event-log"));

    let demo_script = http
        .get(format!("{base_url}/demo/app.js"))
        .send()
        .expect("Web Demo script");
    assert_eq!(demo_script.status(), reqwest::StatusCode::OK);
    let demo_script = demo_script.text().expect("Demo script body");
    assert!(demo_script.contains("conversation_snapshot"));
    assert!(demo_script.contains("event.type === \"text_delta\""));
    assert!(demo_script.contains("list_workspaces"));
    assert!(demo_script.contains("list_attachments"));
    assert!(demo_script.contains("attachment_ids"));
    assert!(demo_script.contains("file_references"));
    assert!(demo_script.contains("archive_session"));
    assert!(demo_script.contains("restore_session"));
    assert!(demo_script.contains("event.type === \"usage_updated\""));
    assert!(demo_script.contains("event.type === \"reasoning_delta\""));
    assert!(demo_script.contains("part.type === \"reasoning\""));
    assert!(demo_script.contains("event.type === \"tool_started\""));
    assert!(demo_script.contains("event.type === \"tool_output\""));
    assert!(demo_script.contains("event.type === \"tool_completed\""));
    assert!(demo_script.contains("event.type === \"model_retry_scheduled\""));
    assert!(demo_script.contains("event.error.code"));
    assert!(demo_script.contains("turn.usage"));
    assert!(demo_script.contains("eventName === \"stream_gap\""));
    assert!(demo_script.contains("refreshAuthoritativeState"));
    assert!(demo_script.contains("requestAnimationFrame(flushUiFrame)"));
    assert!(demo_script.contains("node.appendData(delta)"));
    assert!(demo_script.contains("MAX_RENDERED_HISTORY_MESSAGES"));
    assert!(!demo_script.contains("textContent += event.delta"));
    assert!(demo_script.contains("set_session_variant"));
    assert!(demo_script.contains("set_session_approval_mode"));
    assert!(demo_script.contains("reload_permissions"));
    assert!(demo_script.contains("list_pending_approvals"));
    assert!(demo_script.contains("decide_approval"));
    assert!(demo_script.contains("event.type === \"approval_requested\""));
    assert!(demo_script.contains("byId(\"agent-variant\").value"));
    assert!(demo_script.contains("childTasks.handleEvent(event)"));
    assert!(demo_script.contains("childTasks.setParentConversation(conversation)"));

    let child_task_script = http
        .get(format!("{base_url}/demo/child-tasks.js"))
        .send()
        .expect("Web Demo child task script");
    assert_eq!(child_task_script.status(), reqwest::StatusCode::OK);
    let child_task_script = child_task_script.text().expect("child task script body");
    assert!(child_task_script.contains("list_child_tasks"));
    assert!(child_task_script.contains("cancel_child_task"));
    assert!(demo_script.contains("child_task_conversation_snapshot"));
    assert!(child_task_script.contains("requestAnimationFrame"));
    assert!(child_task_script.contains("liveChildUsage"));
    assert!(!child_task_script.contains("textContent += event.delta"));

    let event_response = http
        .get(format!("{base_url}/events"))
        .bearer_auth(&token)
        .send()
        .expect("SSE subscription");
    assert_eq!(event_response.status(), reqwest::StatusCode::OK);
    let mut client = host.connect();
    let workspace_directory = TempDir::new().expect("user workspace");
    let workspace = client.runtime(
        "register_workspace",
        json!({ "path": workspace_directory.path() }),
    )["workspace"]
        .clone();
    assert_eq!(workspace["lifecycle"], "active");
    let workspace_id = workspace["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_owned();
    assert_eq!(
        client.runtime("list_workspaces", json!({}))["workspaces"]
            .as_array()
            .expect("workspace list")
            .len(),
        1
    );

    let session = client.runtime(
        "create_session",
        json!({
            "title": "HTTP acceptance",
            "model_key": "fixture",
            "workspace_id": workspace_id,
        }),
    )["session"]
        .clone();
    assert_eq!(session["workspace_id"], workspace_id);
    let session_id = session["session_id"].as_str().expect("session id");
    client.runtime(
        "set_session_approval_mode",
        json!({ "session_id": session_id, "approval_mode": "auto" }),
    );
    let source = runtime_home.path().join("upload-source.txt");
    fs::write(&source, "attachment-tool-token-91").expect("write upload source");
    let upload = http
        .post(format!("{base_url}/sessions/{session_id}/attachments"))
        .bearer_auth(&token)
        .multipart(
            reqwest::blocking::multipart::Form::new()
                .file("file", &source)
                .expect("upload part"),
        )
        .send()
        .expect("attachment upload");
    let upload_status = upload.status();
    let upload_body = upload.text().expect("upload body");
    assert_eq!(upload_status, reqwest::StatusCode::OK, "{upload_body}");
    let uploaded: serde_json::Value = serde_json::from_str(&upload_body).expect("upload JSON");
    assert_eq!(uploaded["attachment"]["state"], "ready");
    assert_eq!(uploaded["attachment"]["session_id"], session_id);
    let attachment_id = uploaded["attachment"]["attachment_id"]
        .as_str()
        .expect("attachment id")
        .to_owned();
    let readable_path = uploaded["attachment"]["agent_readable_path"]
        .as_str()
        .expect("readable path");
    assert!(
        fs::symlink_metadata(readable_path)
            .expect("readable view")
            .file_type()
            .is_symlink()
    );
    let duplicate = http
        .post(format!("{base_url}/sessions/{session_id}/attachments"))
        .bearer_auth(&token)
        .multipart(
            reqwest::blocking::multipart::Form::new()
                .file("file", &source)
                .expect("duplicate upload part"),
        )
        .send()
        .expect("same content attachment upload");
    assert_eq!(duplicate.status(), reqwest::StatusCode::OK);
    let duplicate: serde_json::Value = duplicate.json().expect("duplicate upload JSON");
    assert_eq!(duplicate["attachment"]["attachment_id"], attachment_id);
    let renamed_source = runtime_home.path().join("same-content-different-name.txt");
    fs::write(&renamed_source, "attachment-tool-token-91").expect("write differently named source");
    let differently_named = http
        .post(format!("{base_url}/sessions/{session_id}/attachments"))
        .bearer_auth(&token)
        .multipart(
            reqwest::blocking::multipart::Form::new()
                .file("file", &renamed_source)
                .expect("differently named upload part"),
        )
        .send()
        .expect("differently named attachment upload");
    assert_eq!(differently_named.status(), reqwest::StatusCode::OK);
    let differently_named: serde_json::Value = differently_named
        .json()
        .expect("differently named upload JSON");
    assert_ne!(
        differently_named["attachment"]["attachment_id"],
        attachment_id
    );
    fs::write(&source, "changed after upload").expect("change source after upload");
    assert_eq!(
        fs::read_to_string(readable_path).expect("read stable uploaded content"),
        "attachment-tool-token-91"
    );
    let listed = client.runtime("list_attachments", json!({ "session_id": session_id }));
    assert_eq!(
        listed["attachments"].as_array().expect("attachments").len(),
        2
    );
    assert_eq!(listed["attachments"][0]["attachment_id"], attachment_id);
    let failed_upload = http
        .post(format!("{base_url}/sessions/{session_id}/attachments"))
        .bearer_auth(&token)
        .multipart(reqwest::blocking::multipart::Form::new().text("wrong", "not-a-file"))
        .send()
        .expect("invalid attachment upload");
    assert_eq!(failed_upload.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        client.runtime("list_attachments", json!({ "session_id": session_id }))["attachments"]
            .as_array()
            .expect("attachments after failed upload")
            .len(),
        2,
        "a later upload failure must not roll back successful attachments"
    );
    let failed_message = http
        .post(format!("{base_url}/commands"))
        .bearer_auth(&token)
        .json(&json!({
            "request_id": "message-failure-after-upload",
            "command": {
                "scope": "runtime",
                "payload": {
                    "type": "submit_input",
                    "payload": {
                        "session_id": session_id,
                        "message": "   ",
                        "variant": "build",
                        "attachment_ids": [attachment_id]
                    }
                }
            }
        }))
        .send()
        .expect("invalid message after upload");
    assert_eq!(failed_message.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        client.runtime("get_session", json!({ "session_id": session_id }))["session"]["message_count"],
        0
    );
    assert_eq!(
        client.runtime("list_attachments", json!({ "session_id": session_id }))["attachments"]
            .as_array()
            .expect("attachments after failed message")
            .len(),
        2
    );
    let mut event_lines = BufReader::new(event_response).lines();
    let data = loop {
        let line = event_lines.next().expect("SSE event").expect("SSE line");
        if let Some(data) = line.strip_prefix("data: ") {
            break data.to_owned();
        }
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).expect("event JSON")["type"],
        "session_created"
    );
    drop(event_lines);

    let removed =
        client.runtime("remove_workspace", json!({ "workspace_id": workspace_id }))["workspace"]
            .clone();
    assert_eq!(removed["lifecycle"], "removed");
    assert!(
        client.runtime("list_workspaces", json!({}))["workspaces"]
            .as_array()
            .expect("active workspaces")
            .is_empty()
    );
    let frozen_session = client.runtime(
        "get_session",
        json!({ "session_id": session["session_id"] }),
    )["session"]
        .clone();
    assert_eq!(frozen_session["workspace_id"], workspace_id);

    let rejected = http
        .post(format!("{base_url}/commands"))
        .bearer_auth(&token)
        .json(&json!({
            "request_id": "removed-workspace-binding",
            "command": {
                "scope": "runtime",
                "payload": {
                    "type": "create_session",
                    "payload": { "workspace_id": workspace_id }
                }
            }
        }))
        .send()
        .expect("removed workspace binding");
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    let rejected: serde_json::Value = rejected.json().expect("workspace error JSON");
    assert_eq!(rejected["error"]["code"], "workspace_removed");

    let restored = client.runtime(
        "register_workspace",
        json!({ "path": workspace_directory.path() }),
    )["workspace"]
        .clone();
    assert_eq!(restored["workspace_id"], workspace_id);
    assert_eq!(restored["lifecycle"], "active");

    let submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "FILE_REFERENCE_CASE inspect the attached file",
            "variant": "build",
            "attachment_ids": [attachment_id],
            "idempotency_key": "file-reference-first"
        }),
    );
    let first_run_id = submitted["run"]["run_id"]
        .as_str()
        .expect("first file run id")
        .to_owned();
    let first_run = client.wait_for_status(
        "first attachment tool run",
        session_id,
        &first_run_id,
        &["completed"],
    );
    assert_eq!(first_run["text"], "file tool verified");
    assert_eq!(first_run["tools"][0]["tool_name"], "read_file");
    assert_eq!(first_run["tools"][0]["status"], "completed");
    let first_conversation = client.conversation(session_id);
    assert_file_reference_conversation(&first_conversation, "upload-source.txt", readable_path, 1);

    let second_submitted = client.runtime(
        "submit_input",
        json!({
            "session_id": session_id,
            "message": "FILE_REFERENCE_CASE inspect the attached file again",
            "variant": "build",
            "attachment_ids": [attachment_id],
            "idempotency_key": "file-reference-second"
        }),
    );
    let second_run_id = second_submitted["run"]["run_id"]
        .as_str()
        .expect("second file run id")
        .to_owned();
    let second_run = client.wait_for_status(
        "second attachment tool run",
        session_id,
        &second_run_id,
        &["completed"],
    );
    assert_eq!(second_run["text"], "file tool verified");
    assert_eq!(second_run["tools"][0]["tool_name"], "read_file");
    assert_eq!(second_run["tools"][0]["status"], "completed");
    let completed_conversation = client.conversation(session_id);
    assert_file_reference_conversation(
        &completed_conversation,
        "upload-source.txt",
        readable_path,
        2,
    );

    let archived = client.runtime("archive_session", json!({ "session_id": session_id }));
    assert_eq!(archived["session"]["lifecycle"], "archived");

    client.runtime("shutdown_runtime", json!({}));
    let output = host.wait();
    assert!(output.status.success());
    assert_output_is_safe(&output, &token);
    assert!(!runtime_home.path().join("run/runtime.json").exists());

    verify_physical_state(
        runtime_home.path(),
        session_id,
        readable_path,
        2,
        "archived",
    );

    let second_host = HostProcess::start_web_demo(runtime_home.path());
    let second_token = second_host.access_token().to_owned();
    let mut recovered = second_host.connect();
    let sessions = recovered.runtime("list_sessions", json!({ "filter": "all" }));
    assert_eq!(sessions["sessions"].as_array().expect("sessions").len(), 1);
    assert_eq!(sessions["sessions"][0]["session_id"], session_id);
    assert_eq!(sessions["sessions"][0]["lifecycle"], "archived");
    assert_eq!(sessions["sessions"][0]["workspace_id"], workspace_id);
    assert_eq!(
        recovered.runtime("list_workspaces", json!({}))["workspaces"][0]["workspace_id"],
        workspace_id
    );
    let recovered_attachments =
        recovered.runtime("list_attachments", json!({ "session_id": session_id }));
    assert_eq!(
        recovered_attachments["attachments"]
            .as_array()
            .expect("recovered attachments")
            .len(),
        2
    );
    assert_eq!(
        recovered_attachments["attachments"][0]["agent_readable_path"],
        readable_path
    );
    assert_eq!(recovered.conversation(session_id), completed_conversation);

    let restored_session =
        recovered.runtime("restore_session", json!({ "session_id": session_id }));
    assert_eq!(restored_session["session"]["lifecycle"], "active");
    assert_eq!(recovered.conversation(session_id), completed_conversation);
    recovered.runtime("shutdown_runtime", json!({}));
    let second_output = second_host.wait();
    assert!(second_output.status.success());
    assert_output_is_safe(&second_output, &second_token);
    verify_physical_state(runtime_home.path(), session_id, readable_path, 2, "active");
}

fn assert_file_reference_conversation(
    conversation: &serde_json::Value,
    original_name: &str,
    readable_path: &str,
    expected_references: usize,
) {
    let messages = conversation["messages"].as_array().expect("messages");
    let mut message_ids = HashSet::new();
    for message in messages {
        let id = message["turn"]["id"].as_str().expect("message id");
        assert!(
            message_ids.insert(id),
            "Conversation contains duplicate Message ID `{id}`"
        );
    }
    let references = messages
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| {
            message["turn"]["parts"]
                .as_array()
                .expect("User parts")
                .iter()
        })
        .filter(|part| part["type"] == "file_references")
        .collect::<Vec<_>>();
    assert_eq!(references.len(), expected_references);
    for part in references {
        assert_eq!(part["data"]["files"][0]["original_name"], original_name);
        assert_eq!(part["data"]["files"][0]["readable_path"], readable_path);
    }
    let serialized = serde_json::to_string(conversation).expect("serialize conversation");
    assert!(serialized.contains("call-read-file-1"));
    if expected_references > 1 {
        assert!(serialized.contains("call-read-file-2"));
        assert!(serialized.contains("toolmsg_2"));
    }
    assert!(serialized.contains("file tool verified"));
    let latest_assistant = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "assistant")
        .expect("latest Assistant message");
    assert_eq!(latest_assistant["turn"]["usage"]["input_tokens"], 120);
    assert_eq!(latest_assistant["turn"]["usage"]["output_tokens"], 20);
    assert_eq!(latest_assistant["turn"]["usage"]["total_tokens"], 140);
    assert_eq!(latest_assistant["turn"]["usage"]["cached_input_tokens"], 80);
}

fn verify_physical_state(
    runtime_home: &Path,
    session_id: &str,
    readable_path: &str,
    expected_runs: i64,
    expected_lifecycle: &str,
) {
    let database = runtime_home.join("data/runtime.sqlite3");
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open v0.11 acceptance database read-only");
    let (lifecycle, generation, message_count): (String, i64, i64) = connection
        .query_row(
            "SELECT lifecycle, body_generation, message_count FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("session row");
    assert_eq!(lifecycle, expected_lifecycle);
    assert_eq!(message_count, expected_runs * 4);
    for (table, expected) in [
        ("workspaces", 1_i64),
        ("sessions", 1),
        ("attachments", 2),
        ("attachment_blobs", 2),
        ("inputs", expected_runs),
        ("runs", expected_runs),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("table count");
        assert_eq!(count, expected, "unexpected {table} count");
    }
    for table in ["pending_tool_exchanges", "body_appends"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("temporary table count");
        assert_eq!(count, 0, "temporary {table} was not cleared");
    }
    let body = runtime_home.join(format!(
        "data/sessions/{session_id}/conversation.{generation}.jsonl"
    ));
    let body = fs::read_to_string(body).expect("read authoritative v0.11 conversation");
    assert_eq!(body.lines().count() as i64, expected_runs * 4);
    let messages = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse authoritative message"))
        .collect::<Vec<serde_json::Value>>();
    assert_file_reference_conversation(
        &json!({ "messages": messages }),
        "upload-source.txt",
        readable_path,
        expected_runs as usize,
    );
    assert_eq!(
        fs::read_to_string(readable_path).expect("read stable attachment view"),
        "attachment-tool-token-91"
    );
}

fn assert_output_is_safe(output: &std::process::Output, access_token: &str) {
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        access_token,
        TEST_API_KEY,
        "FILE_REFERENCE_CASE",
        "attachment-tool-token-91",
        "file tool verified",
    ] {
        assert!(
            !logs.contains(secret),
            "Host output leaked a sensitive marker"
        );
    }
}
