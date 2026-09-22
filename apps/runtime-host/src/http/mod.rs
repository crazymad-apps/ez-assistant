//! Runtime Host 私有 HTTP transport：只转换跨进程请求、响应与观察事件。
//!
//! `routes` 展示路由及中间件执行顺序；`auth` 实现身份与传输边界，
//! `compatibility` 检查软件版本，其余模块处理对应业务协议。

mod attachments;
mod auth;
mod commands;
mod compatibility;
mod error;
mod events;
mod login;
mod materializations;
mod resources;
mod routes;
mod startup;
pub(crate) mod terminals;
mod web;
pub(crate) use routes::router;
pub(crate) use startup::ReadyServices;
pub(crate) use startup::StartupStateHandle;

use std::sync::Arc;

use assistant_protocol::{
    MIN_COMPATIBLE_VERSION, RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth,
};
use axum::Json;
use tokio_util::sync::CancellationToken;

use crate::access::HostAccessHandle;

pub(crate) const MAX_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;

/// Desktop、Web 与 Client 共用的 Host HTTP 状态。
///
/// 用户业务服务经 `user_services` 按已认证身份获取，不能在此缓存某个“当前用户”。
/// Runtime 持有业务权威状态，本结构只持有 Host 配置、用户域入口与传输资源。
#[derive(Clone)]
pub(crate) struct HttpState {
    pub(crate) domains: Option<Arc<crate::user_domain::UserDomains>>,
    pub(crate) startup: StartupStateHandle,
    access_token: Arc<str>,
    authority: Arc<str>,
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
}

impl HttpEndpointState {
    pub(crate) fn new(
        access_token: &str,
        authority: String,
        base_url: String,
        instance_id: String,
    ) -> Self {
        Self {
            access_token: Arc::from(access_token),
            authority: Arc::from(authority),
            base_url: Arc::from(base_url),
            instance_id: Arc::from(instance_id),
        }
    }
}

impl HttpState {
    async fn user_services(
        &self,
        permit: &crate::access::AccessPermit,
    ) -> Result<Arc<ReadyServices>, error::HttpError> {
        if let Some(domains) = &self.domains {
            return domains
                .ensure(permit)
                .await
                .map_err(error::HttpError::from_domain);
        }
        // 既有私有测试宿主显式发布的服务也经过同一 handler 获取流程。
        permit
            .check()
            .map_err(|_| error::HttpError::unauthorized())?;
        Ok(self.startup.services()?)
    }

    pub(crate) fn starting(
        endpoint: HttpEndpointState,
        shutdown: CancellationToken,
        access: HostAccessHandle,
        terminals: Arc<crate::user_terminal::UserTerminals>,
    ) -> Self {
        let HttpEndpointState {
            access_token,
            authority,
            base_url,
            instance_id,
        } = endpoint;
        Self {
            domains: None,
            terminals,
            file_reads: Arc::new(tokio::sync::Semaphore::new(8)),
            startup: StartupStateHandle::new(),
            access_token,
            authority,
            port: base_url
                .parse::<reqwest::Url>()
                .ok()
                .and_then(|url| url.port_or_known_default())
                .expect("published endpoint has a port"),
            connections: shutdown.child_token(),
            shutdown,
            access,
            instance_id,
            secure: base_url.starts_with("https://"),
        }
    }
}

async fn ensure_ready(
    axum::extract::State(state): axum::extract::State<HttpState>,
    axum::Extension(permit): axum::Extension<crate::access::AccessPermit>,
) -> Result<axum::http::StatusCode, error::HttpError> {
    state.user_services(&permit).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

async fn health(
    axum::extract::State(state): axum::extract::State<HttpState>,
) -> Json<RuntimeHostHealth> {
    Json(state.startup.health())
}

async fn capabilities(
    axum::extract::State(state): axum::extract::State<HttpState>,
    axum::Extension(permit): axum::Extension<Option<crate::access::AccessPermit>>,
) -> Json<RuntimeHostCapabilities> {
    // 只在 capabilities 查询时读取 PATH；不启动命令、不探测 Shell 环境，不进入普通输入路径。
    let rg_on_path = if permit.is_some() {
        tokio::task::spawn_blocking(|| {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|directory| {
                    directory
                        .join(if cfg!(windows) { "rg.exe" } else { "rg" })
                        .is_file()
                })
            })
        })
        .await
        .ok()
    } else {
        None
    };
    Json(RuntimeHostCapabilities {
        mode: state.access.mode(),
        platform: permit.as_ref().map(|_| std::env::consts::OS.to_owned()),
        architecture: permit.as_ref().map(|_| std::env::consts::ARCH.to_owned()),
        rg_on_path,
        min_compatible_version: MIN_COMPATIBLE_VERSION.to_owned(),
        runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        max_command_bytes: MAX_COMMAND_BYTES as u64,
        max_attachment_bytes: Some(MAX_ATTACHMENT_BYTES),
        sse: true,
        streaming_upload: true,
        features: {
            let mut features = vec![
                RuntimeHostFeature::StartupDiagnostics,
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
            ];
            if state.access.center().is_some() {
                features.push(RuntimeHostFeature::EnterpriseIdentity);
            }
            features
        },
    })
}
