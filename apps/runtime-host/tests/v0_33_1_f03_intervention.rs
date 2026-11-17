#![cfg(unix)]

mod support;

use std::{fs, path::Path};

use serde_json::json;
use support::{FakeProvider, HostProcess, write_config};

#[test]
fn intervention_commands_preserve_queue_identity_and_read_back_after_restart() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f03-intervention-")
        .tempdir_in(root)
        .unwrap();
    let provider = FakeProvider::start();
    write_config(
        home.path(),
        provider.endpoint(),
        "f03-isolated-provider-key",
    );
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let session = client.runtime(
        "create_session",
        json!({"title":"F03 intervention fixture"}),
    );
    let session_id = session["session"]["session_id"].as_str().unwrap();
    let direct_request = json!({
        "session_id":session_id, "variant":"build", "message":"FIRST_CASE direct guidance",
        "delivery":"intervention", "idempotency_key":"direct-guidance"
    });
    let mut old = client.submit_started_input(
        json!({"session_id":session_id,"message":"CANCEL_CASE","variant":"build"}),
    );
    client.wait_for_status(
        "original running",
        session_id,
        old["run"]["run_id"].as_str().unwrap(),
        &["running"],
    );
    let direct = client.runtime("submit_input", direct_request.clone());
    assert_eq!(direct["user_message"]["text"], "FIRST_CASE direct guidance");
    assert_eq!(direct["run"]["input_id"], direct["input_id"]);
    client.wait_for_status(
        "original cancelled",
        session_id,
        old["run"]["run_id"].as_str().unwrap(),
        &["cancelled"],
    );
    client.wait_for_status(
        "direct intervention",
        session_id,
        direct["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );

    old = client.submit_started_input(
        json!({"session_id":session_id,"message":"CANCEL_CASE","variant":"build"}),
    );
    client.wait_for_status(
        "second original running",
        session_id,
        old["run"]["run_id"].as_str().unwrap(),
        &["running"],
    );
    let tail = client.runtime(
        "submit_input",
        json!({"session_id":session_id,"message":"QUEUED_AFTER_RESTART ordinary tail","variant":"build"}),
    );
    let target = client.runtime(
        "submit_input",
        json!({"session_id":session_id,"message":"REPLACEMENT_CASE queued guidance","variant":"build"}),
    );
    let converted = client.runtime(
        "intervene_queued_input",
        json!({"session_id":session_id,"input_id":target["input_id"]}),
    );
    assert_eq!(converted["input_id"], target["input_id"]);
    assert_eq!(
        converted["user_message"]["text"],
        "REPLACEMENT_CASE queued guidance"
    );
    client.wait_for_status(
        "converted intervention",
        session_id,
        converted["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    let tail_run = client.wait_for_input_run(session_id, tail["input_id"].as_str().unwrap());
    client.wait_for_status(
        "ordinary tail",
        session_id,
        tail_run["run_id"].as_str().unwrap(),
        &["completed"],
    );
    let view = client.runtime("get_session_view", json!({"session_id":session_id}));
    let items = view["snapshot"]["value"]["conversation"]["items"]
        .as_array()
        .unwrap();
    let user_messages: Vec<_> = items
        .iter()
        .filter(|item| item["type"] == "user")
        .filter_map(|item| item["text"].as_str())
        .collect();
    assert_eq!(
        user_messages,
        [
            "CANCEL_CASE",
            "FIRST_CASE direct guidance",
            "CANCEL_CASE",
            "REPLACEMENT_CASE queued guidance",
            "QUEUED_AFTER_RESTART ordinary tail"
        ]
    );
    drop(host);

    let restarted = HostProcess::start(home.path());
    let mut client = restarted.connect();
    let duplicate = client.runtime("submit_input", direct_request);
    assert_eq!(duplicate["input_id"], direct["input_id"]);
    assert_eq!(duplicate["user_message"], direct["user_message"]);
    assert_eq!(duplicate["run"]["run_id"], direct["run"]["run_id"]);
}
