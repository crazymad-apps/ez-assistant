//! viewer server 端到端链路测试：POST /ingest → SSE /events 广播 → 静态页。

use std::time::Duration;

use agent_core::AgentEvent;
use debug_viewer::{DebugChannel, DebugClient, DebugEnvelope, DebugPayload, router};

/// 在随机端口启动 server，返回 base URL 与后台任务句柄。
async fn spawn_server() -> String {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, router()).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn ingest_is_broadcast_to_sse_subscribers() {
    let base = spawn_server().await;
    let client = reqwest::Client::new();

    // 先建立 SSE 订阅，再推送，避免错过广播。
    let mut sse = client
        .get(format!("{base}/events"))
        .send()
        .await
        .expect("connect sse");
    assert!(sse.status().is_success());

    let envelope = DebugEnvelope {
        ch: debug_viewer::DebugChannel::Llm,
        seq: 0,
        sent_at_ms: 1_752_000_000_000,
        correlation_id: Some("test-1".to_owned()),
        payload: DebugPayload::TurnEstablished {
            model: "deepseek-v4-flash".to_owned(),
            endpoint: "https://api.deepseek.com".to_owned(),
            message_count: 1,
            tool_count: 0,
            elapsed_ms: 42,
        },
    };
    let status = client
        .post(format!("{base}/ingest"))
        .json(&envelope)
        .send()
        .await
        .expect("post ingest")
        .status();
    assert_eq!(status, 204);

    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match sse.chunk().await {
                Ok(Some(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if text.contains("turn_established") {
                        break text;
                    }
                }
                other => panic!("SSE stream ended unexpectedly: {other:?}"),
            }
        }
    })
    .await
    .expect("broadcast frame within timeout");
    assert!(frame.contains("\"model\":\"deepseek-v4-flash\""));
    assert!(frame.contains("\"received_at_ms\":"));
    assert!(frame.contains("\"correlation_id\":\"test-1\""));
}

#[tokio::test]
async fn static_page_and_malformed_ingest_behave() {
    let base = spawn_server().await;
    let client = reqwest::Client::new();

    let html = client
        .get(&base)
        .send()
        .await
        .expect("get index")
        .text()
        .await
        .expect("index body");
    assert!(html.contains("debug viewer"));
    assert!(html.contains("id=\"session-tabs\""));
    assert!(html.contains("id=\"channel-tabs\""));
    assert!(html.contains("data-channel=\"llm\""));
    assert!(html.contains("data-channel=\"agent\""));
    assert!(html.contains("data-channel=\"runtime\""));
    assert!(!html.contains("type=\"checkbox\""));
    assert!(html.contains("app.css?v=v0.3-context"));
    assert!(html.contains("app.js?v=v0.3-context"));

    let js_response = client
        .get(format!("{base}/app.js"))
        .send()
        .await
        .expect("get app.js");
    assert_eq!(
        js_response
            .headers()
            .get("cache-control")
            .expect("cache-control"),
        "no-store"
    );
    // ServeDir 按扩展名推断 MIME，.js → text/javascript（无 charset 参数）。
    assert_eq!(
        js_response
            .headers()
            .get("content-type")
            .expect("content-type"),
        "text/javascript"
    );
    let js = js_response.text().await.expect("app.js body");
    assert!(js.contains("let activeChannel = \"agent\""));
    assert!(js.contains("function switchChannel"));
    assert!(js.contains("function sessionIdOfCorrelation"));
    assert!(js.contains("correlation.lastIndexOf(\"/\")"));
    assert!(js.contains("function addUserMessage"));
    assert!(!js.contains("function lastUserText"));
    assert!(js.contains("function onAgentEvent"));
    assert!(js.contains("usage_updated"));
    assert!(js.contains("execution_compaction_required"));
    assert!(js.contains("function addTimeline"));
    assert!(js.contains("function runtimeTitle"));
    assert!(js.contains("context_window_evaluated"));
    assert!(js.contains("context_compaction_finished"));
    assert!(js.contains("continuation_started"));
    assert!(js.contains("function trimChannelEntries"));
    assert!(js.contains("entries.length - EVENT_CAP"));
    assert!(!js.contains("panel.firstChild.remove()"));

    // 畸形 payload：缺 payload 字段，反序列化失败 → 4xx，不影响后续请求。
    let status = client
        .post(format!("{base}/ingest"))
        .body("{\"ch\":\"llm\"}")
        .header("content-type", "application/json")
        .send()
        .await
        .expect("post malformed")
        .status();
    assert!(status.is_client_error());
}

#[tokio::test]
async fn debug_client_preserves_channels_shared_sequence_and_correlation() {
    // `DebugClient` 成功路径：默认 LLM 与显式通道共用队列、序号和关联 ID。
    let base = spawn_server().await;
    let http = reqwest::Client::new();
    let mut sse = http
        .get(format!("{base}/events"))
        .send()
        .await
        .expect("connect sse");
    assert!(sse.status().is_success());

    let client = DebugClient::new(&base).with_correlation_id("client-e2e");
    client.post(DebugPayload::TurnEstablished {
        model: "deepseek-v4-flash".to_owned(),
        endpoint: "https://api.deepseek.com".to_owned(),
        message_count: 1,
        tool_count: 0,
        elapsed_ms: 7,
    });
    client.post_on(
        DebugChannel::Agent,
        DebugPayload::AgentEvent {
            event: AgentEvent::ExecutionStarted,
        },
    );
    client.post_on(
        DebugChannel::Runtime,
        DebugPayload::RuntimeEvent {
            name: "run_started".to_owned(),
            data: serde_json::json!({"run_id": "run-1"}),
        },
    );

    let frames = tokio::time::timeout(Duration::from_secs(5), async {
        let mut frames = String::new();
        loop {
            match sse.chunk().await {
                Ok(Some(bytes)) => {
                    frames.push_str(&String::from_utf8_lossy(&bytes));
                    if frames.contains("turn_established")
                        && frames.contains("agent_event")
                        && frames.contains("runtime_event")
                    {
                        break frames;
                    }
                }
                other => panic!("SSE stream ended unexpectedly: {other:?}"),
            }
        }
    })
    .await
    .expect("all client frames within timeout");
    assert!(frames.contains("\"ch\":\"llm\",\"seq\":0"));
    assert!(frames.contains("\"ch\":\"agent\",\"seq\":1"));
    assert!(frames.contains("\"ch\":\"runtime\",\"seq\":2"));
    assert_eq!(
        frames.matches("\"correlation_id\":\"client-e2e\"").count(),
        3
    );
    assert!(!client.is_muted());
}
