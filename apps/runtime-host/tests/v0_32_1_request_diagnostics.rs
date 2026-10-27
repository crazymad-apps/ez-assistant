mod support;

use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use support::HostProcess;

#[test]
fn formal_host_correlates_read_phases_without_logging_request_or_session_content() {
    for enabled in [false, true] {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join("config.toml"), "schema_version=1\n").unwrap();
        let host = if enabled {
            HostProcess::start_with_request_diagnostics(home.path())
        } else {
            HostProcess::start(home.path())
        };
        let mut client = host.connect();
        let session = client.runtime(
            "create_session",
            json!({"title":"private-title-must-not-be-logged"}),
        );
        let session_id = session["session"]["session_id"].as_str().unwrap();
        let request_id = "raw-request-secret\nforged=1";
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{}/commands", host.base_url()))
            .bearer_auth(host.access_token())
            .headers(support::compatibility_headers())
            .json(&json!({
                "request_id": request_id,
                "command": {"scope":"runtime", "payload": {
                    "type":"get_session_view", "payload":{"session_id":session_id}
                }},
            }))
            .send()
            .unwrap();
        assert!(response.status().is_success());
        let body: Value = response.json().unwrap();
        assert_eq!(body["request_id"], request_id);
        assert_eq!(
            body["result"]["payload"]["payload"]["snapshot"]["value"]["session"]["session_id"],
            session_id
        );
        client.runtime("shutdown_runtime", json!({}));
        drop(client);
        let output = host.wait();
        assert!(output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("private-title-must-not-be-logged"));
        assert!(!stderr.contains(session_id));
        assert!(!stderr.contains("raw-request-secret"));
        assert!(!stderr.contains("forged=1"));
        let diagnostics = stderr
            .lines()
            .filter(|line| line.starts_with("runtime-request "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!diagnostics.contains(home.path().to_string_lossy().as_ref()));
        if !enabled {
            assert!(!stderr.contains("runtime-request"));
            continue;
        }
        let request_hash = Sha256::digest(request_id.as_bytes());
        let expected = format!(
            "request={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            request_hash[0],
            request_hash[1],
            request_hash[2],
            request_hash[3],
            request_hash[4],
            request_hash[5]
        );
        let correlation = stderr
            .lines()
            .find(|line| line.contains(&expected))
            .expect("client correlation");
        let trace = correlation
            .split_whitespace()
            .find(|part| part.starts_with("trace="))
            .unwrap();
        let lines: Vec<_> = stderr
            .lines()
            .filter(|line| line.split_whitespace().any(|part| part == trace))
            .collect();
        for phase in [
            "request",
            "authentication",
            "user_domain",
            "session_preparation",
            "session_gate",
            "store_queue",
            "store_reply",
            "projection",
            "response",
        ] {
            let stage = format!("stage={phase}");
            for event in ["event=begin", "event=end"] {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.split_whitespace().any(|part| part == stage)
                            && line.contains(event)),
                    "missing {phase} {event}: {lines:?}"
                );
            }
        }
        assert!(
            lines
                .iter()
                .any(|line| line.contains("event=status status=200"))
        );
    }
}
