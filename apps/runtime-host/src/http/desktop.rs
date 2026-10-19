//! 只向当前 Host 的本机客户端提供桌面媒体入口，沿用已有登录和 Origin 核验。
use super::{HttpState, socket_auth::SocketAuthentication};
use crate::access::AccessPermit;
use axum::{
    Extension,
    extract::{ConnectInfo, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
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
    if !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(owner) = state.desktop.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(slot) = owner.sockets.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_message_size(16_384)
        .max_frame_size(16_384)
        .on_upgrade(move |socket| async move {
            let task_owner = owner.clone();
            let auth = SocketAuthentication {
                state,
                headers,
                peer,
                permit,
                target: None,
            };
            if owner
                .submit(Box::pin(async move {
                    let _slot = slot;
                    crate::desktop_media::socket::serve(socket, auth, task_owner).await
                }))
                .is_err()
            {
                eprintln!("runtime-host: desktop admission unavailable");
            }
        })
}
