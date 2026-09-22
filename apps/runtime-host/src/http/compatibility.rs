//! 已认证 HTTP 的软件版本准入；普通 GET 媒体仅复用所属登录的不可变声明。

#[cfg(test)]
mod tests;

use crate::access::AccessPermit;
use assistant_protocol::{
    CLIENT_VERSION_HEADER, ClientCompatibility, MIN_COMPATIBLE_VERSION_HEADER,
    RuntimeCompatibilityError, RuntimeCompatibilityErrorCode, check_compatibility,
};
use axum::{
    Extension, Json,
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(super) fn declaration(
    headers: &HeaderMap,
) -> Result<Option<ClientCompatibility>, RuntimeCompatibilityError> {
    let version = headers
        .get_all(CLIENT_VERSION_HEADER)
        .iter()
        .collect::<Vec<_>>();
    let minimum = headers
        .get_all(MIN_COMPATIBLE_VERSION_HEADER)
        .iter()
        .collect::<Vec<_>>();
    if version.is_empty() && minimum.is_empty() {
        return Ok(None);
    }
    if version.len() != 1 || minimum.len() != 1 {
        return Err(invalid());
    }
    let pair = ClientCompatibility {
        version: version[0].to_str().map_err(|_| invalid())?.into(),
        min_compatible_version: minimum[0].to_str().map_err(|_| invalid())?.into(),
    };
    if !pair.is_valid() {
        return Err(invalid());
    }
    Ok(Some(pair))
}

fn invalid() -> RuntimeCompatibilityError {
    RuntimeCompatibilityError {
        code: RuntimeCompatibilityErrorCode::InvalidDeclaration,
        client: None,
        host: Some(ClientCompatibility::current()),
    }
}

/// 普通 API 每次声明版本；在鉴权之后、正文读取和用户 Runtime 初始化之前执行。
pub(super) async fn require_declaration(request: Request, next: Next) -> Response {
    admit_request(request, next, None).await
}

/// 只挂在浏览器直接加载的 GET 媒体组：图片和下载不能自行添加版本头，允许沿用
/// 当前普通登录保存的声明。原生 bootstrap 没有登录声明，仍需显式头；HEAD 不回退。
pub(super) async fn allow_session_declaration(
    Extension(permit): Extension<Option<AccessPermit>>,
    request: Request,
    next: Next,
) -> Response {
    let fallback = if request.method() == Method::GET {
        permit
            .as_ref()
            .and_then(AccessPermit::compatibility)
            .cloned()
    } else {
        None
    };
    admit_request(request, next, fallback).await
}

async fn admit_request(
    mut request: Request,
    next: Next,
    fallback: Option<ClientCompatibility>,
) -> Response {
    // 显式头存在时只检查显式值；非法或不兼容均不得被登录声明掩盖。
    let client = match declaration(request.headers()) {
        Ok(declared) => declared.or(fallback),
        Err(error) => return response(error),
    };
    if let Err(error) = check_compatibility(client.as_ref(), &ClientCompatibility::current()) {
        return response(error);
    }
    let Some(client) = client else {
        return response(invalid());
    };
    request.extensions_mut().insert(client);
    next.run(request).await
}

pub(super) fn response(error: RuntimeCompatibilityError) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({"error": error})),
    )
        .into_response()
}
