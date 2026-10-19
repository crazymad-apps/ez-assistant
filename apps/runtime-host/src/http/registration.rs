//! 本机管理 transport 只传递意图；设备材料与中心交互留在 HostAccess owner。
use super::HttpState;
use crate::access::AccessPermit;
use assistant_protocol::HostRegistrationCommand;
use axum::{
    Extension, Json,
    extract::State,
    response::{IntoResponse, Response},
};

pub(super) async fn command(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
    Json(command): Json<HostRegistrationCommand>,
) -> Response {
    match state.access.registration(command, permit).await {
        Ok(status) => Json(status).into_response(),
        Err(error) => super::login::access_error(error),
    }
}

/// 与其他 Host 观察流相同的 SSE；不创建用户 Runtime，未登录也可凭本机原生证明订阅。
pub(super) async fn events(State(state): State<HttpState>) -> impl IntoResponse {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let mut receiver = state.access.network.subscribe();
    let stream = async_stream::stream! {
        loop {
            let connection = *receiver.borrow_and_update();
            yield Ok::<_, std::convert::Infallible>(Event::default().event("host_network_changed")
                .data(serde_json::json!({"connection":connection}).to_string()));
            tokio::select! {
                () = state.shutdown.cancelled() => break,
                result = receiver.changed() => { if result.is_err() { break; } },
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 用户级准入沿用既有认证，不要求调用方登记，也不授予本机管理权。
pub(super) async fn access(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
    Json(command): Json<assistant_protocol::HostNetworkCommand>,
) -> Response {
    match state.access.network.command(command, permit).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => super::login::access_error(error),
    }
}
