//! Runtime Host 私有 HTTP transport：只转换跨进程请求、响应与观察事件。

mod attachments;
mod auth;
mod commands;
mod error;
mod events;
mod materializations;
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
    materializations::materialize_session,
    resources::{
        export_session_markdown, list_session_resource_files, preview_attachment,
        preview_child_tool_file, preview_session_resource_file, preview_tool_file,
        resolve_child_tool_file_native_path, resolve_session_resource_native_path,
        resolve_tool_file_native_path, thumbnail_attachment,
    },
};
use crate::{device::DeviceGatewayHandle, speech::SpeechServiceHandle};

pub(crate) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;

/// Desktop HTTP 路由共享的应用状态。
///
/// Runtime 持有业务权威状态，Gateway/Speech 句柄只桥接 Host 子系统；本结构不缓存它们的第二份投影。
#[derive(Clone)]
pub(crate) struct HttpState {
    runtime: Arc<AssistantRuntime>,
    access_token: Arc<str>,
    authority: Arc<str>,
    base_url: Arc<str>,
    upload_staging_directory: Arc<PathBuf>,
    device_gateway: DeviceGatewayHandle,
    speech: SpeechServiceHandle,
    shutdown: CancellationToken,
}

/// HTTP listener 建立前即可冻结的端点与认证配置。
///
/// 它不含 Runtime、Gateway 等服务句柄，便于 Server 先绑定端口再完成应用状态装配。
#[derive(Clone)]
pub(crate) struct HttpEndpointState {
    access_token: Arc<str>,
    authority: Arc<str>,
    base_url: Arc<str>,
    upload_staging_directory: Arc<PathBuf>,
}

impl HttpEndpointState {
    pub(crate) fn new(
        access_token: &str,
        authority: String,
        base_url: String,
        runtime_home: PathBuf,
    ) -> Self {
        Self {
            access_token: Arc::from(access_token),
            authority: Arc::from(authority),
            base_url: Arc::from(base_url),
            upload_staging_directory: Arc::new(runtime_home.join("data/staging/uploads")),
        }
    }
}

impl HttpState {
    pub(crate) fn new(
        runtime: Arc<AssistantRuntime>,
        endpoint: HttpEndpointState,
        device_gateway: DeviceGatewayHandle,
        speech: SpeechServiceHandle,
        shutdown: CancellationToken,
    ) -> Self {
        let HttpEndpointState {
            access_token,
            authority,
            base_url,
            upload_staging_directory,
        } = endpoint;
        Self {
            runtime,
            access_token,
            authority,
            base_url,
            upload_staging_directory,
            device_gateway,
            speech,
            shutdown,
        }
    }
}

pub(crate) fn router(state: HttpState) -> Router {
    let command_route = post(handle_command).layer(DefaultBodyLimit::max(MAX_COMMAND_BYTES));
    let attachment_route = post(upload_attachment).layer(DefaultBodyLimit::disable());
    let materialization_route = post(materialize_session).layer(DefaultBodyLimit::disable());
    let api = Router::new()
        .route("/commands", command_route)
        .route("/session-materializations", materialization_route)
        .route("/sessions/{session_id}/attachments", attachment_route)
        .route(
            "/sessions/{session_id}/attachments/{attachment_id}/preview",
            get(preview_attachment),
        )
        .route(
            "/sessions/{session_id}/attachments/{attachment_id}/thumbnail",
            get(thumbnail_attachment),
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
            "/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/preview",
            get(preview_child_tool_file),
        )
        .route(
            "/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/native-path",
            get(resolve_child_tool_file_native_path),
        )
        .route(
            "/sessions/{session_id}/export.md",
            get(export_session_markdown),
        )
        .route(
            "/sessions/{session_id}/resource-files/list",
            post(list_session_resource_files),
        )
        .route(
            "/sessions/{session_id}/resource-files/preview",
            post(preview_session_resource_file),
        )
        .route(
            "/sessions/{session_id}/resource-files/native-path",
            post(resolve_session_resource_native_path),
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
            RuntimeHostFeature::SessionMaterialization,
            RuntimeHostFeature::SessionResourceFiles,
        ],
    })
}
