//! 用户终端 Upgrade 复用普通 HTTP 来源边界；首帧 token 不进入 URL 或日志。

use super::{HttpState, auth, error::HttpError};
use crate::{
    access::AccessPermit,
    user_terminal::{TerminalOrigin, socket::SocketAuthentication},
};
use assistant_protocol::{
    GetSessionRequest, GetWorkspaceRequest, UserTerminalSource, WorkspaceLifecycle,
};
use axum::{
    Extension,
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::{HeaderMap, Request, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

pub(super) async fn upgrade(
    State(state): State<HttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(permit): Extension<Option<AccessPermit>>,
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

fn authenticate_http(
    auth: &SocketAuthentication,
    bearer: Option<&str>,
) -> Result<AccessPermit, HttpError> {
    if let Some(bearer) = bearer {
        let mut request = Request::new(axum::body::Body::empty());
        *request.headers_mut() = auth.headers.clone();
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {bearer}")
                .parse()
                .map_err(|_| HttpError::unauthorized())?,
        );
        request.extensions_mut().insert(ConnectInfo(auth.peer));
        let permit = auth::authorize_request(&auth.state, &request)?
            .1
            .ok_or_else(HttpError::unauthorized)?;
        if let Some(initial) = &auth.permit {
            initial.check().map_err(|_| HttpError::unauthorized())?;
            if initial.native != permit.native || initial.login_id() != permit.login_id() {
                return Err(HttpError::unauthorized());
            }
        }
        Ok(permit)
    } else {
        auth.permit
            .clone()
            .filter(|permit| permit.check().is_ok())
            .ok_or_else(HttpError::unauthorized)
    }
}

pub(crate) fn authenticate(
    auth: &SocketAuthentication,
    bearer: Option<&str>,
) -> Result<AccessPermit, crate::user_terminal::TerminalError> {
    authenticate_http(auth, bearer)
        .map_err(|_| crate::user_terminal::failure("登录已失效，请重新登录。"))
}

pub(crate) fn validate_compatibility(
    headers: &HeaderMap,
    client: Option<&assistant_protocol::ClientCompatibility>,
) -> Result<(), assistant_protocol::RuntimeCompatibilityError> {
    use assistant_protocol::{
        ClientCompatibility, RuntimeCompatibilityError, RuntimeCompatibilityErrorCode,
        check_compatibility,
    };
    let host = ClientCompatibility::current();
    let declared = super::compatibility::declaration(headers)?;
    if let Some(header) = &declared {
        check_compatibility(Some(header), &host)?;
    }
    check_compatibility(client, &host)?;
    if declared
        .as_ref()
        .is_some_and(|header| Some(header) != client)
    {
        return Err(RuntimeCompatibilityError {
            code: RuntimeCompatibilityErrorCode::InvalidDeclaration,
            client: client.cloned(),
            host: Some(host),
        });
    }
    Ok(())
}

pub(crate) async fn directory(
    state: &HttpState,
    source: &UserTerminalSource,
) -> Result<(TerminalOrigin, std::path::PathBuf), crate::user_terminal::TerminalError> {
    let fail = || crate::user_terminal::failure("终端来源或启动目录已不可用。");
    let (origin, directory) = match source {
        UserTerminalSource::Session {
            session_id,
            locator,
        } => {
            let session = state
                .startup
                .services()
                .map_err(|_| fail())?
                .runtime
                .get_session(GetSessionRequest {
                    session_id: session_id.clone(),
                })
                .await
                .map_err(|_| fail())?
                .session;
            let root = state
                .startup
                .services()
                .map_err(|_| fail())?
                .runtime
                .resolve_session_resource_root(session_id, &locator.root)
                .await
                .map_err(|_| fail())?;
            let (_, directory) = super::resources::resolve_session_resource_path(&root, locator)
                .await
                .map_err(|_| fail())?;
            (
                TerminalOrigin {
                    session: Some(session_id.clone()),
                    workspace: session.workspace_id,
                },
                directory,
            )
        }
        UserTerminalSource::Workspace { workspace_id } => {
            let workspace = state
                .startup
                .services()
                .map_err(|_| fail())?
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
                    session: None,
                    workspace: Some(workspace_id.clone()),
                },
                workspace.user_directory.into(),
            )
        }
    };
    if let Some(id) = &origin.workspace {
        let workspace = state
            .startup
            .services()
            .map_err(|_| fail())?
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
    if !tokio::fs::metadata(&directory)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(fail());
    }
    Ok((origin, directory))
}

#[cfg(test)]
mod compatibility_tests {
    use super::validate_compatibility;
    use assistant_protocol::{ClientCompatibility, RuntimeCompatibilityErrorCode as Code};
    use axum::http::HeaderMap;

    #[test]
    fn frame_declaration_is_required_even_with_valid_headers() {
        let own = ClientCompatibility::current();
        let mut headers = HeaderMap::new();
        headers.insert(
            assistant_protocol::CLIENT_VERSION_HEADER,
            own.version.parse().unwrap(),
        );
        headers.insert(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            own.min_compatible_version.parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, None).unwrap_err().code,
            Code::MissingDeclaration
        );
        assert!(validate_compatibility(&headers, Some(&own)).is_ok());
        let newer = ClientCompatibility {
            version: "0.25.3".into(),
            ..own
        };
        assert_eq!(
            validate_compatibility(&headers, Some(&newer))
                .unwrap_err()
                .code,
            Code::InvalidDeclaration
        );
        assert!(validate_compatibility(&HeaderMap::new(), Some(&newer)).is_ok());
    }

    #[test]
    fn incompatible_or_partial_headers_cannot_be_hidden_by_the_frame() {
        let own = ClientCompatibility::current();
        let mut headers = HeaderMap::new();
        headers.insert(
            assistant_protocol::CLIENT_VERSION_HEADER,
            "0.25.1".parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, Some(&own))
                .unwrap_err()
                .code,
            Code::InvalidDeclaration
        );
        headers.insert(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            "0.25.1".parse().unwrap(),
        );
        assert_eq!(
            validate_compatibility(&headers, Some(&own))
                .unwrap_err()
                .code,
            Code::ClientTooOld
        );
        let newer_floor = ClientCompatibility {
            version: "0.25.3".into(),
            min_compatible_version: "0.25.3".into(),
        };
        assert_eq!(
            validate_compatibility(&HeaderMap::new(), Some(&newer_floor))
                .unwrap_err()
                .code,
            Code::HostTooOld
        );
    }
}
