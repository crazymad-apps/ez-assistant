//! Runtime Host 私有 HTTP transport：只转换跨进程请求、响应与观察事件。

mod attachments;
mod auth;
mod commands;
mod error;
mod events;
#[cfg(feature = "web-demo")]
mod web_demo;

use std::{path::PathBuf, sync::Arc};

use assistant_protocol::{
    PROTOCOL_VERSION, RuntimeHostCapabilities, RuntimeHostHealth, RuntimeHostHealthStatus,
};
use assistant_runtime::AssistantRuntime;
use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use tokio_util::sync::CancellationToken;

use self::{
    attachments::upload_attachment, auth::authorize, commands::handle_command,
    events::stream_events,
};

pub(crate) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct HttpState {
    runtime: Arc<AssistantRuntime>,
    access_token: Arc<str>,
    authority: Arc<str>,
    base_url: Arc<str>,
    upload_staging_directory: Arc<PathBuf>,
    shutdown: CancellationToken,
    web_demo: bool,
}

impl HttpState {
    pub(crate) fn new(
        runtime: Arc<AssistantRuntime>,
        access_token: &str,
        authority: String,
        base_url: String,
        runtime_home: PathBuf,
        shutdown: CancellationToken,
        web_demo: bool,
    ) -> Self {
        Self {
            runtime,
            access_token: Arc::from(access_token),
            authority: Arc::from(authority),
            base_url: Arc::from(base_url),
            upload_staging_directory: Arc::new(runtime_home.join("data/staging/uploads")),
            shutdown,
            web_demo,
        }
    }
}

pub(crate) fn router(state: HttpState) -> Router {
    let command_route = post(handle_command).layer(DefaultBodyLimit::max(MAX_COMMAND_BYTES));
    let attachment_route = post(upload_attachment).layer(DefaultBodyLimit::disable());
    let api = Router::new()
        .route("/commands", command_route)
        .route("/sessions/{session_id}/attachments", attachment_route)
        .route("/events", get(stream_events))
        .route("/health", get(health))
        .route("/capabilities", get(capabilities))
        .layer(middleware::from_fn_with_state(state.clone(), authorize));

    #[cfg(feature = "web-demo")]
    let api = if state.web_demo {
        api.merge(web_demo::router(state.clone()))
    } else {
        api
    };

    api.with_state(state)
}

async fn health() -> Json<RuntimeHostHealth> {
    Json(RuntimeHostHealth {
        status: RuntimeHostHealthStatus::Ready,
    })
}

async fn capabilities(
    axum::extract::State(state): axum::extract::State<HttpState>,
) -> Json<RuntimeHostCapabilities> {
    Json(RuntimeHostCapabilities {
        protocol_version: PROTOCOL_VERSION,
        runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        max_command_bytes: MAX_COMMAND_BYTES as u64,
        max_attachment_bytes: Some(MAX_ATTACHMENT_BYTES),
        sse: true,
        streaming_upload: true,
        private_web_demo: state.web_demo,
    })
}
