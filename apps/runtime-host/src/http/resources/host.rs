//! 任意 Host 路径与 Session 下载入口。共用文件读取，不扩大会话 locator 的授权范围。
use super::{files, invalid_request, resource_error, resource_unavailable};
use crate::http::HttpState;
use assistant_protocol::{
    HostFileRequest, ListHostFilesRequest, PreviewSessionResourceFileRequest, RuntimeErrorInfo,
    SessionId,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path as RoutePath, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use std::{fs::File, path::Path};
use tokio::io::AsyncReadExt;

/// 阻塞文件工作随请求有界提交；permit 移入闭包，HTTP 取消也不会提前释放并发名额。
pub(super) async fn read<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, RuntimeErrorInfo> + Send + 'static,
) -> Result<T, RuntimeErrorInfo> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| resource_unavailable())?
}
pub(super) fn permit(
    state: &HttpState,
) -> Result<tokio::sync::OwnedSemaphorePermit, RuntimeErrorInfo> {
    state
        .file_reads
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid_request("文件读取繁忙，请稍后重试。"))
}

pub(in crate::http) async fn list_host_files(
    State(state): State<HttpState>,
    Json(request): Json<ListHostFilesRequest>,
) -> Response {
    let permit = match permit(&state) {
        Ok(p) => p,
        Err(e) => return resource_error(e),
    };
    match read(move || {
        let _permit = permit;
        let path = files::host_path(request.path.as_deref())?;
        files::list(&path, None, request.include_hidden, true)
    })
    .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => resource_error(e),
    }
}
pub(in crate::http) async fn select_host_directory(
    State(state): State<HttpState>,
    Json(request): Json<HostFileRequest>,
) -> Response {
    let permit = match permit(&state) {
        Ok(p) => p,
        Err(e) => return resource_error(e),
    };
    match read(move || {
        let _permit = permit;
        let path = files::host_path(Some(&request.path))?;
        files::open_resolved(&path, true)?;
        Ok(HostFileRequest {
            path: files::path_text(&path)?,
        })
    })
    .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => resource_error(e),
    }
}
pub(in crate::http) async fn preview_host_file(
    State(state): State<HttpState>,
    Json(request): Json<HostFileRequest>,
) -> Response {
    let permit = match permit(&state) {
        Ok(p) => p,
        Err(e) => return resource_error(e),
    };
    match read(move || {
        let _permit = permit;
        files::preview(&files::host_path(Some(&request.path))?)
    })
    .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => resource_error(e),
    }
}
pub(in crate::http) async fn download_host_file(
    State(state): State<HttpState>,
    Json(request): Json<HostFileRequest>,
) -> Response {
    let permit = match permit(&state) {
        Ok(p) => p,
        Err(e) => return resource_error(e),
    };
    match read(move || {
        let path = files::host_path(Some(&request.path))?;
        Ok((files::open_resolved(&path, false)?, path, permit))
    })
    .await
    {
        Ok((file, path, permit)) => download(file, &path, permit),
        Err(e) => resource_error(e),
    }
}
pub(in crate::http) async fn download_session_file(
    State(state): State<HttpState>,
    RoutePath(session_id): RoutePath<String>,
    Json(request): Json<PreviewSessionResourceFileRequest>,
) -> Response {
    let id = match SessionId::new(session_id) {
        Ok(id) => id,
        Err(_) => return resource_error(invalid_request("会话标识无效。")),
    };
    let root = match state
        .runtime
        .resolve_session_resource_root(&id, &request.locator.root)
        .await
    {
        Ok(root) => root,
        Err(e) => return resource_error(e.to_protocol_info()),
    };
    let (_, path) = match super::resolve_session_resource_path(&root, &request.locator).await {
        Ok(path) => path,
        Err(e) => return resource_error(e),
    };
    let permit = match permit(&state) {
        Ok(p) => p,
        Err(e) => return resource_error(e),
    };
    match read(move || Ok((files::open_resolved(&path, false)?, path, permit))).await {
        Ok((file, path, permit)) => download(file, &path, permit),
        Err(e) => resource_error(e),
    }
}
fn download(file: File, path: &Path, permit: tokio::sync::OwnedSemaphorePermit) -> Response {
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => return resource_error(files::error(e)),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let encoded: String = name
        .as_bytes()
        .iter()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    let mut file = tokio::fs::File::from_std(file);
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut remaining = size;
        let mut buffer = vec![0; super::STREAM_CHUNK_BYTES];
        while remaining > 0 {
            let length = remaining.min(buffer.len() as u64) as usize;
            match file.read(&mut buffer[..length]).await {
                Ok(0) => { yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "文件在下载期间缩短。")); break; }
                Ok(n) => { remaining -= n as u64; yield Ok(Bytes::copy_from_slice(&buffer[..n])); }
                Err(e) => { yield Err(e); break; }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"download\"; filename*=UTF-8''{encoded}"
    )) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&size.to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    response
}

/// 下载只接受已登录客户端提供的资源身份，再由 Runtime 解析权威文件路径。
pub(in crate::http) async fn download_attachment(
    State(state): State<HttpState>,
    RoutePath((session, attachment)): RoutePath<(String, String)>,
) -> Response {
    use assistant_protocol::{AttachmentId, AttachmentState, GetAttachmentRequest};
    let request = match (SessionId::new(session), AttachmentId::new(attachment)) {
        (Ok(session_id), Ok(attachment_id)) => GetAttachmentRequest {
            session_id,
            attachment_id,
        },
        _ => return resource_error(invalid_request("附件标识无效。")),
    };
    let attachment = match state.runtime.get_attachment(request).await {
        Ok(result) if result.attachment.state == AttachmentState::Ready => result.attachment,
        Ok(_) => return resource_error(resource_unavailable()),
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    download_path(
        &state,
        attachment.agent_readable_path,
        attachment.original_name,
    )
    .await
}
pub(in crate::http) async fn download_tool_file(
    State(state): State<HttpState>,
    RoutePath((session, message, resource)): RoutePath<(String, String, String)>,
) -> Response {
    let Some((owner, message, resource)) =
        super::main_tool_resource_request(&session, &message, &resource)
    else {
        return resource_error(invalid_request("工具资源标识无效。"));
    };
    download_tool(state, owner, message, resource).await
}
pub(in crate::http) async fn download_child_tool_file(
    State(state): State<HttpState>,
    RoutePath((session, child, message, resource)): RoutePath<(String, String, String, String)>,
) -> Response {
    let Some((owner, message, resource)) =
        super::child_tool_resource_request(&session, &child, &message, &resource)
    else {
        return resource_error(invalid_request("工具资源标识无效。"));
    };
    download_tool(state, owner, message, resource).await
}
async fn download_tool(
    state: HttpState,
    owner: assistant_protocol::ConversationOwner,
    message: assistant_protocol::MessageId,
    resource: assistant_protocol::ResourceRefId,
) -> Response {
    let resource_id = resource.clone();
    match state
        .runtime
        .resolve_tool_file_resource(&owner, &message, &resource)
        .await
    {
        Ok(resource)
            if resource.origin == assistant_protocol::ToolFileResourceOrigin::SessionToolImage =>
        {
            // 复用预览对产品私有图片的内容哈希校验，下载不绕过该资源契约。
            let mut response = super::preview_tool_resource(
                state,
                owner,
                message,
                resource_id,
                axum::http::HeaderMap::new(),
            )
            .await;
            if response.status().is_success() {
                response.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("attachment"),
                );
                response.headers_mut().insert(
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                );
            }
            response
        }
        Ok(resource) => download_path(&state, resource.path, resource.display_name).await,
        Err(error) => resource_error(error.to_protocol_info()),
    }
}
async fn download_path(state: &HttpState, path: String, name: String) -> Response {
    let permit = match permit(state) {
        Ok(permit) => permit,
        Err(error) => return resource_error(error),
    };
    match read(move || {
        let path = files::host_path(Some(&path))?;
        Ok((files::open_resolved(&path, false)?, permit))
    })
    .await
    {
        Ok((file, permit)) => download(file, Path::new(&name), permit),
        Err(error) => resource_error(error),
    }
}
