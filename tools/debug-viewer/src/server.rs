//! viewer server：HTTP 端点与广播。
//!
//! - `POST /ingest`：接收 [`DebugEnvelope`]，附上接收时刻后广播给所有 SSE 订阅者。
//! - `GET /events`：SSE 广播流（落后太多的订阅者收到 `gap` 事件）。
//! - 其余 GET 路径：`ServeDir` 直接伺服 `public/` 目录（运行时读盘，页面改动刷新浏览器即生效）。
//!
//! 只绑定 loopback；本服务是独立开发工具，不属于产品进程。

use std::{convert::Infallible, net::Ipv4Addr, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware::{self, Next},
    response::{Response, Sse, sse::Event, sse::KeepAlive},
    routing::{get, post},
};
use futures_core::Stream;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

use crate::wire::{BroadcastMessage, DEFAULT_PORT, DebugEnvelope, now_ms};

/// 广播容量；落后于此数量的订阅者收到 `gap` 事件而不是逐条补偿。
const BROADCAST_CAPACITY: usize = 256;

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<String>,
}

/// 构造 viewer 的 HTTP 路由（独立出来便于测试）。
pub fn router() -> Router {
    let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
    Router::new()
        .route("/events", get(events))
        .route("/ingest", post(ingest))
        // 静态页运行时从源码树读盘：页面改动刷新浏览器即生效，无需重编译。
        // 根路径在编译期固定为本 crate 目录；二进制挪到别的机器会失去页面（开发工具可接受）。
        .fallback_service(ServeDir::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/public"
        )))
        // 开发查看器必须在普通刷新后立即读取源码树中的最新静态资源。
        .layer(middleware::from_fn(disable_cache))
        .with_state(AppState { tx })
}

/// 在 loopback 上启动 viewer server，直到进程被杀。
pub async fn run(port: u16) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    eprintln!("debug viewer: http://localhost:{port}");
    eprintln!("推送端：POST http://localhost:{port}/ingest");
    axum::serve(listener, router()).await
}

/// 从环境读取端口：`DEBUG_PORT`，缺省 [`DEFAULT_PORT`]。
pub fn port_from_env() -> u16 {
    std::env::var("DEBUG_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

async fn ingest(State(state): State<AppState>, Json(envelope): Json<DebugEnvelope>) -> StatusCode {
    let message = BroadcastMessage {
        envelope,
        received_at_ms: now_ms(),
    };
    if let Ok(json) = serde_json::to_string(&message) {
        // 没有浏览器订阅时 send 返回 Err，属正常情况。
        let _ = state.tx.send(json);
    }
    StatusCode::NO_CONTENT
}

async fn disable_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.tx.subscribe();
    let stream = stream! {
        loop {
            match rx.recv().await {
                Ok(json) => yield Ok(Event::default().data(json)),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    yield Ok(Event::default().event("gap").data(skipped.to_string()));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
