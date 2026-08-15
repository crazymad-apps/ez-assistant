//! 受控附件预览与 Session Markdown 导出资源路由。

use std::{io, path::Path, str::FromStr};

use assistant_protocol::{
    AttachmentId, AttachmentState, ConversationOwner, GetAttachmentRequest, MessageId,
    ResourceRefId, RuntimeErrorCode, RuntimeErrorInfo, SessionId,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path as RoutePath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use super::{HttpState, error::runtime_status};

const MAX_TEXT_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
struct ResourceErrorBody {
    error: RuntimeErrorInfo,
}

#[derive(Serialize)]
pub(super) struct NativeResourcePath {
    path: String,
    display_name: String,
}

pub(super) async fn preview_attachment(
    State(state): State<HttpState>,
    RoutePath((session_id, attachment_id)): RoutePath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("session id is invalid")),
    };
    let attachment_id = match AttachmentId::new(attachment_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("attachment id is invalid")),
    };
    let attachment = match state.runtime.get_attachment(GetAttachmentRequest {
        session_id,
        attachment_id,
    }) {
        Ok(result) => result.attachment,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    if attachment.state != AttachmentState::Ready {
        return resource_error(RuntimeErrorInfo::new(
            RuntimeErrorCode::AttachmentUnavailable,
            "attachment is unavailable",
        ));
    }
    preview_path(
        Path::new(&attachment.agent_readable_path),
        &attachment.original_name,
        headers,
    )
    .await
}

pub(super) async fn preview_tool_file(
    State(state): State<HttpState>,
    RoutePath((session_id, message_id, resource_ref_id)): RoutePath<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        tool_resource_request(&session_id, &message_id, &resource_ref_id)
    else {
        return resource_error(invalid_request("tool resource identity is invalid"));
    };
    let resource = match state
        .runtime
        .resolve_tool_file_resource(&owner, &message_id, &resource_ref_id)
        .await
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    preview_path(Path::new(&resource.path), &resource.display_name, headers).await
}

pub(super) async fn resolve_tool_file_native_path(
    State(state): State<HttpState>,
    RoutePath((session_id, message_id, resource_ref_id)): RoutePath<(String, String, String)>,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        tool_resource_request(&session_id, &message_id, &resource_ref_id)
    else {
        return resource_error(invalid_request("tool resource identity is invalid"));
    };
    let resource = match state
        .runtime
        .resolve_tool_file_resource(&owner, &message_id, &resource_ref_id)
        .await
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    let resolved_path = match resolve_regular_file(Path::new(&resource.path)).await {
        Ok(value) => value,
        Err(error) => return resource_error(error),
    };
    Json(NativeResourcePath {
        path: resolved_path.to_string_lossy().into_owned(),
        display_name: resource.display_name,
    })
    .into_response()
}

fn tool_resource_request(
    session_id: &str,
    message_id: &str,
    resource_ref_id: &str,
) -> Option<(ConversationOwner, MessageId, ResourceRefId)> {
    let session_id = SessionId::new(session_id.to_owned()).ok()?;
    Some((
        ConversationOwner::MainSession { session_id },
        MessageId::new(message_id.to_owned()).ok()?,
        ResourceRefId::new(resource_ref_id.to_owned()).ok()?,
    ))
}

async fn preview_path(path: &Path, name: &str, headers: HeaderMap) -> Response {
    let media_type = match preview_media_type(name) {
        Some(value) => value,
        None => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceNotPreviewable,
                "resource type is not previewable",
            ));
        }
    };
    // Attachment readable paths may be Runtime-created, content-addressed links. Resolve the
    // authoritative resource path once, then only stream a regular target file.
    let resolved_path = match resolve_regular_file(path).await {
        Ok(value) => value,
        Err(error) => return resource_error(error),
    };
    let metadata = match tokio::fs::metadata(&resolved_path).await {
        Ok(value) => value,
        Err(_) => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentUnavailable,
                "resource is unavailable",
            ));
        }
    };
    let size = metadata.len();
    let max_size = if media_type.starts_with("image/") {
        MAX_IMAGE_PREVIEW_BYTES
    } else {
        MAX_TEXT_PREVIEW_BYTES
    };
    let range = match parse_range(headers.get(header::RANGE), size, max_size) {
        Ok(value) => value,
        Err(info) => return resource_error(info),
    };
    let (start, end, status) = match range {
        Some((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
        None if size <= max_size => (0, size.saturating_sub(1), StatusCode::OK),
        None => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceTooLarge,
                "resource exceeds the preview size limit",
            ));
        }
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut file = match tokio::fs::File::open(&resolved_path).await {
        Ok(value) => value,
        Err(_) => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentUnavailable,
                "attachment is unavailable",
            ));
        }
    };
    if start > 0 && file.seek(io::SeekFrom::Start(start)).await.is_err() {
        return resource_error(RuntimeErrorInfo::new(
            RuntimeErrorCode::AttachmentUnavailable,
            "attachment is unavailable",
        ));
    }
    let stream = async_stream::stream! {
        let mut remaining = length;
        let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
        while remaining > 0 {
            let limit = usize::try_from(remaining.min(STREAM_CHUNK_BYTES as u64))
                .unwrap_or(STREAM_CHUNK_BYTES);
            let read = match file.read(&mut buffer[..limit]).await {
                Ok(value) => value,
                Err(error) => {
                    yield Err::<Bytes, io::Error>(error);
                    break;
                }
            };
            if read == 0 {
                break;
            }
            remaining -= read as u64;
            yield Ok::<Bytes, io::Error>(Bytes::copy_from_slice(&buffer[..read]));
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        let value = format!("bytes {start}-{end}/{size}");
        if let Ok(value) = HeaderValue::from_str(&value) {
            response_headers.insert(header::CONTENT_RANGE, value);
        }
    }
    response
}

async fn resolve_regular_file(path: &Path) -> Result<std::path::PathBuf, RuntimeErrorInfo> {
    let resolved_path = tokio::fs::canonicalize(path).await.map_err(|_| {
        RuntimeErrorInfo::new(
            RuntimeErrorCode::AttachmentUnavailable,
            "resource is unavailable",
        )
    })?;
    match tokio::fs::metadata(&resolved_path).await {
        Ok(value) if value.is_file() => Ok(resolved_path),
        _ => Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::AttachmentUnavailable,
            "resource is unavailable",
        )),
    }
}

pub(super) async fn export_session_markdown(
    State(state): State<HttpState>,
    RoutePath(session_id): RoutePath<String>,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("session id is invalid")),
    };
    let markdown = match state.runtime.export_session_markdown(&session_id).await {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    let mut response = Response::new(Body::from(markdown));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment"),
    );
    response
}

fn preview_media_type(name: &str) -> Option<&'static str> {
    let extension = Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)?
        .to_ascii_lowercase();
    match extension.as_str() {
        "txt" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "css" | "scss" | "toml" | "yaml"
        | "yml" | "xml" | "csv" => Some("text/plain; charset=utf-8"),
        "md" | "markdown" => Some("text/markdown; charset=utf-8"),
        "json" => Some("application/json; charset=utf-8"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn parse_range(
    header: Option<&HeaderValue>,
    size: u64,
    max_size: u64,
) -> Result<Option<(u64, u64)>, RuntimeErrorInfo> {
    let Some(header) = header else {
        return Ok(None);
    };
    let value = header
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("bytes="))
        .ok_or_else(|| invalid_request("range header is invalid"))?;
    if value.contains(',') || size == 0 {
        return Err(invalid_request("range header is invalid"));
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| invalid_request("range header is invalid"))?;
    let start = u64::from_str(start).map_err(|_| invalid_request("range header is invalid"))?;
    let end = if end.is_empty() {
        start
            .saturating_add(max_size.saturating_sub(1))
            .min(size - 1)
    } else {
        u64::from_str(end).map_err(|_| invalid_request("range header is invalid"))?
    };
    if start >= size || end < start || end >= size || end - start + 1 > max_size {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceTooLarge,
            "requested range is invalid or exceeds the preview limit",
        ));
    }
    Ok(Some((start, end)))
}

fn resource_error(error: RuntimeErrorInfo) -> Response {
    (
        runtime_status(error.code),
        Json(ResourceErrorBody { error }),
    )
        .into_response()
}

fn invalid_request(message: &'static str) -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(RuntimeErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_allowlist_excludes_html_and_svg() {
        assert_eq!(
            preview_media_type("notes.md"),
            Some("text/markdown; charset=utf-8")
        );
        assert_eq!(preview_media_type("photo.PNG"), Some("image/png"));
        assert_eq!(preview_media_type("page.html"), None);
        assert_eq!(preview_media_type("vector.svg"), None);
    }

    #[test]
    fn range_is_single_and_bounded() {
        let header = HeaderValue::from_static("bytes=10-19");
        assert_eq!(
            parse_range(Some(&header), 100, 20).expect("range"),
            Some((10, 19))
        );
        let multiple = HeaderValue::from_static("bytes=0-1,4-5");
        assert!(parse_range(Some(&multiple), 100, 20).is_err());
        let oversized = HeaderValue::from_static("bytes=0-30");
        assert!(parse_range(Some(&oversized), 100, 20).is_err());
    }
}
