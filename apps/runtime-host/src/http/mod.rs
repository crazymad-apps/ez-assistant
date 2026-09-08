//! Runtime Host 私有 HTTP transport：只转换跨进程请求、响应与观察事件。

mod attachments;
mod auth;
mod commands;
mod error;
mod events;
mod login;
mod materializations;
mod resources;
pub(crate) mod terminals;
mod web;

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
use crate::{access::HostAccessHandle, device::DeviceGatewayHandle, speech::SpeechServiceHandle};

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
    upload_staging_directory: Arc<PathBuf>,
    device_gateway: DeviceGatewayHandle,
    speech: SpeechServiceHandle,
    shutdown: CancellationToken,
    pub(crate) access: HostAccessHandle,
    instance_id: Arc<str>,
    port: u16,
    secure: bool,
    connections: CancellationToken,
    file_reads: Arc<tokio::sync::Semaphore>,
    pub(crate) terminals: Arc<crate::user_terminal::UserTerminals>,
}

/// HTTP listener 建立前即可冻结的端点与认证配置。
///
/// 它不含 Runtime、Gateway 等服务句柄，便于 Server 先绑定端口再完成应用状态装配。
#[derive(Clone)]
pub(crate) struct HttpEndpointState {
    access_token: Arc<str>,
    authority: Arc<str>,
    base_url: Arc<str>,
    instance_id: Arc<str>,
    upload_staging_directory: Arc<PathBuf>,
}

impl HttpEndpointState {
    pub(crate) fn new(
        access_token: &str,
        authority: String,
        base_url: String,
        runtime_home: PathBuf,
        instance_id: String,
    ) -> Self {
        Self {
            access_token: Arc::from(access_token),
            authority: Arc::from(authority),
            base_url: Arc::from(base_url),
            instance_id: Arc::from(instance_id),
            upload_staging_directory: Arc::new(runtime_home.join("data/staging/uploads")),
        }
    }
}

impl HttpState {
    /// Host 设置与 Runtime 模型设置共用文件，提交后刷新唯一配置投影的 revision。
    pub(crate) async fn refresh_configuration_projection(
        &self,
    ) -> Result<(), crate::access::AccessError> {
        self.runtime
            .reload_config(assistant_protocol::ReloadConfigRequest::default())
            .await
            .map(|_| ())
            .map_err(|_| crate::access::AccessError::Unavailable)
    }

    pub(crate) fn new(
        runtime: Arc<AssistantRuntime>,
        endpoint: HttpEndpointState,
        device_gateway: DeviceGatewayHandle,
        speech: SpeechServiceHandle,
        shutdown: CancellationToken,
        access: HostAccessHandle,
        terminals: Arc<crate::user_terminal::UserTerminals>,
    ) -> Self {
        let HttpEndpointState {
            access_token,
            authority,
            base_url,
            instance_id,
            upload_staging_directory,
        } = endpoint;
        Self {
            terminals,
            file_reads: Arc::new(tokio::sync::Semaphore::new(8)),
            runtime,
            access_token,
            authority,
            port: base_url
                .parse::<reqwest::Url>()
                .ok()
                .and_then(|url| url.port_or_known_default())
                .expect("published endpoint has a port"),
            upload_staging_directory,
            device_gateway,
            speech,
            connections: shutdown.child_token(),
            shutdown,
            access,
            instance_id,
            secure: base_url.starts_with("https://"),
        }
    }
}

pub(crate) fn router(state: HttpState) -> Router {
    let command_route = post(handle_command).layer(DefaultBodyLimit::max(MAX_COMMAND_BYTES));
    let attachment_route = post(upload_attachment).layer(DefaultBodyLimit::disable());
    let materialization_route = post(materialize_session).layer(DefaultBodyLimit::disable());
    let api = Router::new()
        .route("/auth/login", post(login::login).layer(DefaultBodyLimit::max(4096)))
        .route("/auth/logout", post(login::logout))
        .route("/auth/session", get(login::session))
        .route("/commands", command_route)
        .route("/sessions/{session_id}/attachments/{attachment_id}/download", get(resources::download_attachment))
        .route("/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/download", get(resources::download_tool_file))
        .route("/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/download", get(resources::download_child_tool_file))
        .route("/host-files/list", post(resources::list_host_files))
        .route("/host-files/select-directory", post(resources::select_host_directory))
        .route("/host-files/preview", post(resources::preview_host_file))
        .route("/host-files/download", post(resources::download_host_file))
        .route("/sessions/{session_id}/resource-files/download", post(resources::download_session_file))
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
        .route("/user-terminals/socket", get(terminals::upgrade))
        .route("/events", get(stream_events))
        .route("/health", get(health))
        .route("/capabilities", get(capabilities))
        .layer(middleware::from_fn_with_state(state.clone(), authorize));

    let pages = Router::new()
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authorize_page,
        ));
    api.merge(pages).with_state(state)
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
            RuntimeHostFeature::HostAccess,
            RuntimeHostFeature::WebLogin,
            RuntimeHostFeature::UserTerminals,
        ],
    })
}
