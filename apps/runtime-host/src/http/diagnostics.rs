//! 可显式开启的 Command ingress 阶段诊断；不解析或复制请求正文。

use std::sync::atomic::{AtomicU64, Ordering};

use assistant_runtime::request_diagnostics::{record_response_status, scope_request_diagnostics};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use super::HttpState;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub(super) async fn observe(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    if request.uri().path() != "/commands"
        || std::env::var_os("EZ_ASSISTANT_REQUEST_DIAGNOSTICS").as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
        return next.run(request).await;
    }
    // 客户端 request_id 还在未解析的正文内；先用实例内独立身份覆盖鉴权等待，
    // handler 解码后再写摘要关联，不能为了诊断提前消费正文或信任凭据。
    let scope = format!(
        "{}:{}",
        state.instance_id,
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    scope_request_diagnostics(&scope, async {
        let response = next.run(request).await;
        record_response_status(response.status().as_u16());
        response
    })
    .await
}
