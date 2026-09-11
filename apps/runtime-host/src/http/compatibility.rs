//! 已认证 HTTP 的软件版本准入；普通 GET 媒体仅复用所属登录的不可变声明。

#[cfg(test)]
mod tests;

use crate::access::AccessPermit;
use assistant_protocol::{
    CLIENT_VERSION_HEADER, ClientCompatibility, MIN_COMPATIBLE_VERSION_HEADER,
    RuntimeCompatibilityError, RuntimeCompatibilityErrorCode, check_compatibility,
};
use axum::{
    Json,
    http::{HeaderMap, Method, StatusCode},
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

pub(super) fn admit(
    headers: &HeaderMap,
    method: &Method,
    route: &str,
    permit: Option<&AccessPermit>,
) -> Result<ClientCompatibility, RuntimeCompatibilityError> {
    let declared = declaration(headers)?;
    // 显式头优先，即使不兼容也不回退到登录声明；白名单之外必须每次携带头。
    let client = declared.or_else(|| {
        if method == Method::GET && media_route(route) {
            permit.and_then(AccessPermit::compatibility).cloned()
        } else {
            None
        }
    });
    check_compatibility(client.as_ref(), &ClientCompatibility::current())?;
    client.ok_or_else(invalid)
}

fn media_route(route: &str) -> bool {
    matches!(
        route,
        "/sessions/{session_id}/attachments/{attachment_id}/download"
            | "/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/download"
            | "/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/download"
            | "/sessions/{session_id}/attachments/{attachment_id}/preview"
            | "/sessions/{session_id}/attachments/{attachment_id}/thumbnail"
            | "/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/preview"
            | "/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/preview"
            | "/sessions/{session_id}/export.md"
    )
}

pub(super) fn response(error: RuntimeCompatibilityError) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({"error": error})),
    )
        .into_response()
}
