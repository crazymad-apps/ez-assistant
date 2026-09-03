//! 双 Transport 的开始后断流、超时和用户取消：均不得自动重放真实调用。

use super::fixture::CallBehavior;
use super::*;

#[test]
fn formal_host_mcp_disconnect_timeout_and_cancel_never_replay() {
    for transport in ["stdio", "http"] {
        for (behavior, cancel) in [
            (CallBehavior::Disconnect, false),
            (CallBehavior::Hang, false),
            (CallBehavior::Hang, true),
        ] {
            let home = TempDir::new().expect("isolated fault home");
            let fixture = WireFixture::with_behavior("target", true, String::new(), behavior);
            configure(home.path(), &fixture, "openai_chat_completions", transport);
            let host = HostProcess::start(home.path());
            let mut client = host.connect();
            let session = client.runtime(
                "create_session",
                json!({"title":"MCP fault","model_key":"fixture"}),
            );
            let session_id = session["session"]["session_id"]
                .as_str()
                .expect("session id");
            let submitted = client.runtime("submit_input", json!({"session_id":session_id,"message":"Read a fixture value","variant":"build","mcp_server_key":"target"}));
            let run_id = submitted["run"]["run_id"].as_str().expect("run id");
            let approval = wait_approval(&mut client, session_id);
            client.runtime("decide_approval", json!({"session_id":session_id,"approval_id":approval["approval_id"],"decision":"allow_once"}));
            if cancel {
                // 等到远端真正收到请求再取消，不能用建连前取消代替活动调用取消。
                let deadline = Instant::now() + Duration::from_secs(3);
                while call_count(&fixture, home.path(), transport) == 0 {
                    assert!(Instant::now() < deadline, "remote call did not start");
                    thread::sleep(Duration::from_millis(5));
                }
                client.runtime(
                    "interrupt_run",
                    json!({"session_id":session_id,"run_id":run_id}),
                );
            }
            client.wait_for_status(
                "MCP injected failure",
                session_id,
                run_id,
                &[if cancel { "cancelled" } else { "completed" }],
            );
            let conversation = client.conversation(session_id);
            assert_clean(conversation.to_string().as_bytes());
            assert!(!conversation.to_string().contains("called:first_tool"));
            if !cancel {
                let requests = fixture.state.requests.lock().expect("model requests");
                assert_eq!(requests.len(), 2, "failure returned to the same model loop");
                assert!(requests[1].to_string().contains("remote_may_have_executed"));
            }
            assert_eq!(call_count(&fixture, home.path(), transport), 1);
            client.runtime("shutdown_runtime", json!({}));
            drop(client);
            let output = host.wait();
            assert!(output.status.success());
            assert_clean(&output.stdout);
            assert_clean(&output.stderr);
            scan_data(&home.path().join("data"));
            assert_eq!(
                call_count(&fixture, home.path(), transport),
                1,
                "shutdown must not replay a call"
            );
            eprintln!(
                "MCP fault verified: {transport}, {}, cancel={cancel}",
                behavior.as_str()
            );
        }
    }
}

fn call_count(fixture: &WireFixture, home: &Path, transport: &str) -> usize {
    if transport == "http" {
        fixture.state.calls.load(Ordering::SeqCst)
    } else {
        fs::read_to_string(home.join("fixture-calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
