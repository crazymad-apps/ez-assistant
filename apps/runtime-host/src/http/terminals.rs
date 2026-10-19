//! 用户终端 Upgrade 复用普通 HTTP 来源边界；首帧 token 不进入 URL 或日志。

use super::{HttpState, error::HttpError};
use crate::{
    access::AccessPermit, http::socket_auth::SocketAuthentication, user_terminal::TerminalOrigin,
};
use assistant_protocol::{
    GetSessionRequest, GetWorkspaceRequest, UserTerminalSource, WorkspaceLifecycle,
};
use axum::{
    Extension,
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

/// 已登录资源组完成账号、版本及 Ready 准入后查询；阻塞探测共享有界资源读取预算。
pub(super) async fn shells(
    State(state): State<HttpState>,
) -> Result<axum::Json<Vec<assistant_protocol::UserTerminalShellEntry>>, HttpError> {
    let slot = state
        .file_reads
        .clone()
        .try_acquire_owned()
        .map_err(|_| HttpError::unavailable())?;
    let entries = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        crate::user_terminal::shell_catalog()
    })
    .await
    .map_err(|_| HttpError::unavailable())?;
    Ok(axum::Json(entries))
}

#[derive(Clone)]
pub(super) struct Target(pub String);

pub(super) async fn upgrade(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(permit): Extension<Option<AccessPermit>>,
    target: Option<Extension<Target>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let connection_slot = match state.terminals.sockets.clone().try_acquire_owned() {
        Ok(slot) => slot,
        Err(_) => {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "终端连接过多，请稍后重试。",
            )
                .into_response();
        }
    };
    let slot = match state.terminals.handshakes.clone().try_acquire_owned() {
        Ok(slot) => slot,
        Err(_) => {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "终端连接繁忙，请稍后重试。",
            )
                .into_response();
        }
    };
    ws.max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .on_upgrade(move |socket| async move {
            let owner = state.terminals.clone();
            let auth = SocketAuthentication {
                state,
                headers,
                peer,
                permit,
                target: target.map(|Extension(target)| target.0),
            };
            if owner
                .submit(Box::pin(async move {
                    // 持有到包括错误响应和 Close 握手在内的整个连接结束，限制慢关闭连接。
                    let _connection_slot = connection_slot;
                    crate::user_terminal::socket::serve(socket, auth, slot).await
                }))
                .is_err()
            {
                eprintln!("runtime-host: terminal admission unavailable");
            }
        })
}

pub(crate) async fn directory(
    services: &super::ReadyServices,
    permit: &AccessPermit,
    source: &UserTerminalSource,
) -> Result<(TerminalOrigin, std::path::PathBuf), crate::user_terminal::TerminalError> {
    let fail = || crate::user_terminal::failure("终端来源或启动目录已不可用。");
    let (origin, directory) = match source {
        UserTerminalSource::Session {
            session_id,
            locator,
        } => {
            let session = services
                .runtime
                .get_session(GetSessionRequest {
                    session_id: session_id.clone(),
                })
                .await
                .map_err(|_| fail())?
                .session;
            let root = services
                .runtime
                .resolve_session_resource_root(session_id, &locator.root)
                .await
                .map_err(|_| fail())?;
            let (_, directory) = super::resources::resolve_session_resource_path(&root, locator)
                .await
                .map_err(|_| fail())?;
            (
                TerminalOrigin {
                    user: permit.user_key().cloned(),
                    session: Some(session_id.clone()),
                    workspace: session.workspace_id,
                },
                directory,
            )
        }
        UserTerminalSource::Workspace { workspace_id } => {
            let workspace = services
                .runtime
                .get_workspace(GetWorkspaceRequest {
                    workspace_id: workspace_id.clone(),
                })
                .map_err(|_| fail())?
                .workspace;
            if workspace.lifecycle != WorkspaceLifecycle::Active {
                return Err(fail());
            }
            (
                TerminalOrigin {
                    user: permit.user_key().cloned(),
                    session: None,
                    workspace: Some(workspace_id.clone()),
                },
                workspace
                    .user_directory
                    .unwrap_or(workspace.agent_directory)
                    .into(),
            )
        }
    };
    if let Some(id) = &origin.workspace {
        let workspace = services
            .runtime
            .get_workspace(GetWorkspaceRequest {
                workspace_id: id.clone(),
            })
            .map_err(|_| fail())?
            .workspace;
        if workspace.lifecycle != WorkspaceLifecycle::Active {
            return Err(fail());
        }
    }
    let paths = services.paths.clone();
    let directory = tokio::task::spawn_blocking(move || paths.resolve(&directory, false))
        .await
        .map_err(|_| fail())?
        .map_err(|_| fail())?;
    if !tokio::fs::metadata(&directory)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(fail());
    }
    Ok((origin, directory))
}

pub(crate) async fn services(
    state: &HttpState,
    permit: &AccessPermit,
) -> Result<std::sync::Arc<super::ReadyServices>, crate::user_terminal::TerminalError> {
    state
        .user_services(permit)
        .await
        .map_err(|_| crate::user_terminal::failure("用户服务暂不可用。"))
}
