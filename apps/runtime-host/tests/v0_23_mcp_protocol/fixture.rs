//! 离线模型 wire 与认证 MCP fixture。只记录有界测试事实，复用既有服务器生命周期。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};

use crate::support::FakeProvider;

pub(super) const SECRET: &str = "m8-mcp-credential-must-not-leak-9273";
pub(super) const MODEL_SECRET: &str = "m8-provider-credential-must-not-leak-6138";

pub(super) struct WireFixture {
    pub server: FakeProvider,
    pub state: Arc<WireState>,
}

pub(super) struct WireState {
    pub requests: Mutex<Vec<Value>>,
    pub calls: AtomicUsize,
    pub methods: Mutex<Vec<String>>,
    target: &'static str,
    selected: bool,
    pub image: String,
    pub behavior: CallBehavior,
}

#[derive(Clone, Copy)]
pub(super) enum CallBehavior {
    Reply,
    Disconnect,
    Hang,
}

impl CallBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::Disconnect => "disconnect",
            Self::Hang => "hang",
        }
    }
}

impl WireFixture {
    pub fn start(target: &'static str, selected: bool, image: String) -> Self {
        Self::with_behavior(target, selected, image, CallBehavior::Reply)
    }

    pub fn with_behavior(
        target: &'static str,
        selected: bool,
        image: String,
        behavior: CallBehavior,
    ) -> Self {
        let state = Arc::new(WireState {
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            methods: Mutex::new(Vec::new()),
            target,
            selected,
            image,
            behavior,
        });
        let router = Router::new()
            .route("/v1/chat/completions", post(model))
            .route("/v1/responses", post(model))
            .route(
                "/v1/mcp",
                post(mcp)
                    .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                    .delete(|| async { StatusCode::OK }),
            )
            .with_state(state.clone());
        Self {
            server: FakeProvider::with_router(router),
            state,
        }
    }
}

async fn model(
    State(state): State<Arc<WireState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], format!("Bearer {MODEL_SECRET}"));
    {
        let mut requests = state.requests.lock().expect("request capture");
        assert!(requests.len() < 8, "unexpected unbounded model loop");
        requests.push(body.clone());
    }
    let responses = body.get("input").is_some();
    let items = if responses {
        &body["input"]
    } else {
        &body["messages"]
    };
    let has_result = |id: &str| {
        items.as_array().expect("wire items").iter().any(|item| {
            if responses {
                item["type"] == "function_call_output" && item["call_id"] == id
            } else {
                item["role"] == "tool" && item["tool_call_id"] == id
            }
        })
    };
    let (id, call) = if has_result("m8-invoke") {
        ("m8-answer", None)
    } else if !state.selected && !has_result("m8-discover") {
        (
            "m8-discover",
            Some((
                "discover_mcp_tools",
                json!({"server": state.target, "detail":"full"}),
            )),
        )
    } else {
        (
            "m8-invoke",
            Some((
                "call_mcp_tool",
                json!({"server":state.target,"tool":"first_tool","arguments":{"value":"m8-roundtrip"}}),
            )),
        )
    };
    // 最终答复只有收到真实 Tool Result 才会返回；断言结果正文由测试调用方完成。
    let events = if responses {
        responses_events(id, call)
    } else {
        chat_events(id, call)
    };
    (
        [("content-type", "text/event-stream")],
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
        .into_response()
}

fn chat_events(id: &str, call: Option<(&str, Value)>) -> Vec<Value> {
    let (delta, finish) = match call {
        Some((name, arguments)) => (
            json!({"role":"assistant","tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]}),
            "tool_calls",
        ),
        None => (
            json!({"role":"assistant","content":"MCP fixture completed"}),
            "stop",
        ),
    };
    vec![
        json!({"id":id,"model":"fixture","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"id":id,"model":"fixture","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":40,"completion_tokens":10,"total_tokens":50}}),
    ]
}

fn responses_events(id: &str, call: Option<(&str, Value)>) -> Vec<Value> {
    let item_id = format!("item-{id}");
    let mut events = vec![
        json!({"type":"response.created","response":{"id":id,"model":"fixture","status":"in_progress"}}),
    ];
    if let Some((name, arguments)) = call {
        events.extend([
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":item_id,"type":"function_call","call_id":id,"name":name,"arguments":"","status":"in_progress"}}),
            json!({"type":"response.function_call_arguments.done","item_id":item_id,"output_index":0,"name":name,"arguments":arguments.to_string()}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"id":item_id,"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string(),"status":"completed"}}),
        ]);
    } else {
        events.extend([
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":item_id,"type":"message","role":"assistant","content":[]}}),
            json!({"type":"response.output_text.done","item_id":item_id,"output_index":0,"content_index":0,"text":"MCP fixture completed"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"id":item_id,"type":"message","role":"assistant","content":[{"type":"output_text","text":"MCP fixture completed","annotations":[]}]}}),
        ]);
    }
    events.push(json!({"type":"response.completed","response":{"id":id,"model":"fixture","status":"completed","usage":{"input_tokens":40,"output_tokens":10,"total_tokens":50}}}));
    events
}

async fn mcp(
    State(state): State<Arc<WireState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some(SECRET)
    {
        // 模拟不可信服务器在失败正文中回显认证材料，Host 错误边界必须仍脱敏。
        return (StatusCode::UNAUTHORIZED, SECRET).into_response();
    }
    let method = body["method"].as_str().expect("MCP method");
    state
        .methods
        .lock()
        .expect("method capture")
        .push(method.to_owned());
    let result = match method {
        "server/discover" => return ([("content-type", "text/event-stream")], format!("data: {}\n\n", json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32601,"message":"use initialize"}}))).into_response(),
        "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"m8-http","version":"1"}}),
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => json!({"tools":[{"name":"first_tool","description":"Returns fixture text and image","inputSchema":{"type":"object","required":["value"],"properties":{"value":{"type":"string"}}}}]}),
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(body["params"]["name"], "first_tool");
            assert_eq!(body["params"]["arguments"], json!({"value":"m8-roundtrip"}));
            let fault_body = match state.behavior {
                CallBehavior::Reply => None,
                CallBehavior::Disconnect => Some(Body::from_stream(futures_util::stream::once(async {
                    Err::<Vec<u8>, _>(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
                }))),
                CallBehavior::Hang => Some(Body::from_stream(futures_util::stream::pending::<Result<Vec<u8>, std::io::Error>>())),
            };
            if let Some(body) = fault_body {
                return Response::builder().header("content-type", "text/event-stream").body(body).expect("fault response");
            }
            json!({"content":[{"type":"text","text":"called:first_tool"},{"type":"image","mimeType":"image/png","data":state.image}],"isError":false})
        }
        "notifications/cancelled" => return StatusCode::ACCEPTED.into_response(),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":body["id"],"result":result}).to_string(),
        ))
        .expect("MCP response")
}
