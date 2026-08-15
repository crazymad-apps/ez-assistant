//! Runtime Host 私有 HTTP transport：只转换跨进程请求、响应与观察事件。

mod attachments;
mod auth;
mod commands;
mod error;
mod events;
mod resources;

use std::{path::PathBuf, sync::Arc};

use assistant_protocol::{
    PROTOCOL_VERSION, RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth,
    RuntimeHostHealthStatus,
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
    attachments::upload_attachment,
    auth::authorize,
    commands::handle_command,
    events::stream_events,
    resources::{
        export_session_markdown, preview_attachment, preview_tool_file,
        resolve_tool_file_native_path,
    },
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
}

impl HttpState {
    pub(crate) fn new(
        runtime: Arc<AssistantRuntime>,
        access_token: &str,
        authority: String,
        base_url: String,
        runtime_home: PathBuf,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            runtime,
            access_token: Arc::from(access_token),
            authority: Arc::from(authority),
            base_url: Arc::from(base_url),
            upload_staging_directory: Arc::new(runtime_home.join("data/staging/uploads")),
            shutdown,
        }
    }
}

pub(crate) fn router(state: HttpState) -> Router {
    let command_route = post(handle_command).layer(DefaultBodyLimit::max(MAX_COMMAND_BYTES));
    let attachment_route = post(upload_attachment).layer(DefaultBodyLimit::disable());
    let api = Router::new()
        .route("/commands", command_route)
        .route("/sessions/{session_id}/attachments", attachment_route)
        .route(
            "/sessions/{session_id}/attachments/{attachment_id}/preview",
            get(preview_attachment),
        )
        .route(
            "/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/preview",
            get(preview_tool_file),
        )
        .route(
            "/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/native-path",
            get(resolve_tool_file_native_path),
        )
        .route(
            "/sessions/{session_id}/export.md",
            get(export_session_markdown),
        )
        .route("/events", get(stream_events))
        .route("/health", get(health))
        .route("/capabilities", get(capabilities))
        .layer(middleware::from_fn_with_state(state.clone(), authorize));

    api.with_state(state)
}

async fn health() -> Json<RuntimeHostHealth> {
    Json(RuntimeHostHealth {
        status: RuntimeHostHealthStatus::Ready,
    })
}

async fn capabilities() -> Json<RuntimeHostCapabilities> {
    Json(RuntimeHostCapabilities {
        protocol_version: PROTOCOL_VERSION,
        runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        max_command_bytes: MAX_COMMAND_BYTES as u64,
        max_attachment_bytes: Some(MAX_ATTACHMENT_BYTES),
        sse: true,
        streaming_upload: true,
        features: vec![
            RuntimeHostFeature::EventEnvelopes,
            RuntimeHostFeature::ApplicationSnapshot,
            RuntimeHostFeature::SessionView,
            RuntimeHostFeature::ChildTaskView,
            RuntimeHostFeature::ConversationPaging,
            RuntimeHostFeature::ToolDetail,
            RuntimeHostFeature::QueueControl,
            RuntimeHostFeature::ApprovalQueue,
            RuntimeHostFeature::SessionManagement,
        ],
    })
}
