//! 使用正式 HTTP Adapter 和 Auto 生命周期复现 SSE 握手错误及降级，不另建测试握手流程。

use std::{sync::Mutex, time::Duration};

use axum::{Json, Router, body::Body, http::Response, routing::post};
use rmcp::model::ErrorCode;
use serde_json::{Value, json};

use super::*;
use crate::mcp::http_client::BoundedHttpClient;

async fn connect(uri: String) -> Result<ClientService, McpConnectionError> {
    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("client");
    let mut config = StreamableHttpClientTransportConfig::with_uri(uri)
        .max_sse_event_size(MAX_MESSAGE_BYTES)
        .reinit_on_expired_session(false);
    config.retry_config = Arc::new(NeverRetry::default());
    serve(
        StreamableHttpClientTransport::with_client(BoundedHttpClient(client), config),
        CancellationToken::new(),
    )
    .await
}

fn sse(message: Value) -> Response<Body> {
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(format!(
            ": keepalive\r\n\r\nevent: message\r\ndata: {message}\r\n\r\n"
        )))
        .expect("SSE response")
}

#[tokio::test]
async fn sse_handshake_error_falls_back_and_calls_tool_without_replay() {
    let methods = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = methods.clone();
    let router = Router::new().route("/mcp", post(move |Json(request): Json<Value>| {
        let observed = observed.clone();
        async move {
            let method = request["method"].as_str().expect("method");
            observed.lock().expect("methods").push(method.to_owned());
            let result = match method {
                "server/discover" => return sse(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method is not available"}})),
                "initialize" => return sse(json!({"jsonrpc":"2.0","id":request["id"],"result":{
                    "protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"legacy-sse-fixture","version":"1"}
                }})),
                "notifications/initialized" => return Response::builder().status(202).body(Body::empty()).expect("accepted"),
                "tools/list" => json!({"tools":[{"name":"lookup","description":"Read-only fixture","inputSchema":{"type":"object"}}]}),
                "tools/call" => json!({"content":[{"type":"text","text":"fixture result"}]}),
                other => panic!("unexpected method {other}"),
            };
            Response::builder().header("content-type", "application/json")
                .body(Body::from(json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string())).expect("JSON response")
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let uri = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let service = connect(uri).await?;
        let connection = HostMcpConnection {
            service: RwLock::new(Some(service)),
        };
        let listed = connection
            .list_tools_page(None, CancellationToken::new())
            .await;
        let called = connection
            .call_tool_once(
                "lookup".to_owned(),
                serde_json::Map::new(),
                CancellationToken::new(),
            )
            .await;
        let closed = connection.close(CancellationToken::new()).await;
        Ok::<_, McpConnectionError>((listed, called, closed))
    })
    .await;
    server.abort();
    assert!(server.await.expect_err("server aborted").is_cancelled());
    let (listed, called, closed) = result
        .expect("handshake timeout")
        .expect("Auto must fall back");
    assert_eq!(listed.expect("tools").tools[0].name, "lookup");
    assert!(
        matches!(called.expect("call").content.as_slice(), [McpRawContent::Text { text }] if text == "fixture result")
    );
    closed.expect("close");
    assert_eq!(
        *methods.lock().expect("methods"),
        [
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call"
        ]
    );
}

#[tokio::test]
async fn sse_modern_rejection_or_uncorrelated_error_does_not_downgrade() {
    for (code, wrong_id) in [
        (ErrorCode::HEADER_MISMATCH, false),
        (ErrorCode::METHOD_NOT_FOUND, true),
    ] {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = calls.clone();
        let router = Router::new().route("/mcp", post(move |Json(request): Json<Value>| {
            observed.lock().expect("calls").push(request["method"].as_str().expect("method").to_owned());
            async move { sse(json!({"jsonrpc":"2.0","id":if wrong_id { json!("unrelated") } else { request["id"].clone() },"error":{"code":code,"message":"fixture rejection"}})) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listen");
        let uri = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let result = tokio::time::timeout(Duration::from_secs(2), connect(uri)).await;
        server.abort();
        assert!(server.await.expect_err("server aborted").is_cancelled());
        assert!(result.expect("must reject promptly").is_err());
        assert_eq!(*calls.lock().expect("calls"), ["server/discover"]);
    }
}

#[tokio::test]
#[ignore = "contacts public Microsoft Learn; run explicitly, no credentials or user data"]
async fn microsoft_learn_http_negotiates_and_lists_tools() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let service = connect("https://learn.microsoft.com/api/mcp".to_owned())
            .await
            .expect("Microsoft Learn Auto handshake");
        let connection = HostMcpConnection {
            service: RwLock::new(Some(service)),
        };
        let tools = connection
            .list_tools_page(None, CancellationToken::new())
            .await;
        let closed = connection.close(CancellationToken::new()).await;
        let names = tools
            .expect("list Microsoft tools")
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "microsoft_docs_search"));
        assert!(names.iter().any(|name| name == "microsoft_docs_fetch"));
        assert!(
            names
                .iter()
                .any(|name| name == "microsoft_code_sample_search")
        );
        closed.expect("close Microsoft connection");
    })
    .await
    .expect("Microsoft Learn smoke timeout");
}
