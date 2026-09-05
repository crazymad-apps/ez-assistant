//! 受控附件预览与 Session Markdown 导出资源路由。

use std::{
    cmp::Ordering,
    io,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use assistant_protocol::{
    AttachmentId, AttachmentState, ConversationOwner, GetAttachmentRequest,
    ListSessionResourceFilesRequest, ListSessionResourceFilesResult, MessageId,
    PreviewSessionResourceFileRequest, PreviewSessionResourceFileResult, ResourceRefId,
    RuntimeErrorCode, RuntimeErrorInfo, SessionId, SessionResourceEntry, SessionResourceEntryKind,
    SessionResourceEntryState, SessionResourceLocator, SessionResourcePreviewKind,
    ToolFileResourceOrigin,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path as RoutePath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use super::{HttpState, error::runtime_status};

const MAX_TEXT_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PDF_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 2_000;
const MAX_IMAGE_EDGE: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

const GENERATED_DIRECTORIES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".build",
    "DerivedData",
    "coverage",
];

#[derive(Serialize)]
struct ResourceErrorBody {
    error: RuntimeErrorInfo,
}

#[derive(Serialize)]
pub(super) struct NativeResourcePath {
    path: String,
    display_name: String,
}

pub(super) async fn list_session_resource_files(
    State(state): State<HttpState>,
    RoutePath(session_id): RoutePath<String>,
    Json(request): Json<ListSessionResourceFilesRequest>,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("session id is invalid")),
    };
    let root = match state
        .runtime
        .resolve_session_resource_root(&session_id, &request.locator.root)
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    let (canonical_root, directory) =
        match resolve_session_resource_path(&root, &request.locator).await {
            Ok(value) => value,
            Err(error) => return resource_error(error),
        };
    if !directory.is_dir() {
        return resource_error(invalid_request("resource locator is not a directory"));
    }
    match read_directory_entries(
        &canonical_root,
        &directory,
        &request.locator,
        request.include_hidden,
        request.include_generated,
    )
    .await
    {
        Ok(result) => Json(result).into_response(),
        Err(error) => resource_error(error),
    }
}

pub(super) async fn preview_session_resource_file(
    State(state): State<HttpState>,
    RoutePath(session_id): RoutePath<String>,
    Json(request): Json<PreviewSessionResourceFileRequest>,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("session id is invalid")),
    };
    let root = match state
        .runtime
        .resolve_session_resource_root(&session_id, &request.locator.root)
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    let (_, path) = match resolve_session_resource_path(&root, &request.locator).await {
        Ok(value) => value,
        Err(error) => return resource_error(error),
    };
    match read_session_resource_preview(&path).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => resource_error(error),
    }
}

pub(super) async fn resolve_session_resource_native_path(
    State(state): State<HttpState>,
    RoutePath(session_id): RoutePath<String>,
    Json(locator): Json<SessionResourceLocator>,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return resource_error(invalid_request("session id is invalid")),
    };
    let root = match state
        .runtime
        .resolve_session_resource_root(&session_id, &locator.root)
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    let (_, path) = match resolve_session_resource_path(&root, &locator).await {
        Ok(value) => value,
        Err(error) => return resource_error(error),
    };
    let display_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("")
        .to_owned();
    Json(NativeResourcePath {
        path: path.to_string_lossy().into_owned(),
        display_name,
    })
    .into_response()
}

async fn resolve_session_resource_path(
    root: &str,
    locator: &SessionResourceLocator,
) -> Result<(PathBuf, PathBuf), RuntimeErrorInfo> {
    let relative = normalize_relative_path(&locator.relative_path)?;
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| resource_unavailable())?;
    let candidate = canonical_root.join(relative);
    let resolved = tokio::fs::canonicalize(candidate)
        .await
        .map_err(|_| resource_unavailable())?;
    if !resolved.starts_with(&canonical_root) {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::OperationNotAllowed,
            "resource is outside the authorized root",
        ));
    }
    Ok((canonical_root, resolved))
}

fn normalize_relative_path(value: &str) -> Result<PathBuf, RuntimeErrorInfo> {
    if value.contains('\0') {
        return Err(invalid_request("resource path is invalid"));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(invalid_request("resource path must be relative"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_request("resource path cannot leave its root"));
            }
        }
    }
    Ok(normalized)
}

async fn read_directory_entries(
    canonical_root: &Path,
    directory: &Path,
    parent: &SessionResourceLocator,
    include_hidden: bool,
    include_generated: bool,
) -> Result<ListSessionResourceFilesResult, RuntimeErrorInfo> {
    let mut reader = tokio::fs::read_dir(directory)
        .await
        .map_err(|_| resource_unavailable())?;
    let mut entries = Vec::new();
    let mut truncated = false;
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|_| resource_unavailable())?
    {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let hidden = name.starts_with('.');
        let generated = GENERATED_DIRECTORIES.contains(&name.as_str());
        if (!include_hidden && hidden) || (!include_generated && generated) {
            continue;
        }
        if entries.len() == MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(inspect_directory_entry(canonical_root, entry.path(), parent, name).await);
    }
    entries.sort_by(compare_resource_entries);
    Ok(ListSessionResourceFilesResult { entries, truncated })
}

async fn inspect_directory_entry(
    canonical_root: &Path,
    path: PathBuf,
    parent: &SessionResourceLocator,
    display_name: String,
) -> SessionResourceEntry {
    let relative_path = if parent.relative_path.is_empty() {
        display_name.clone()
    } else {
        format!(
            "{}/{}",
            parent.relative_path.trim_end_matches('/'),
            display_name
        )
    };
    let locator = SessionResourceLocator {
        root: parent.root.clone(),
        relative_path,
    };
    let hidden = display_name.starts_with('.');
    let generated = GENERATED_DIRECTORIES.contains(&display_name.as_str());
    let link_metadata = tokio::fs::symlink_metadata(&path).await;
    let is_symbolic_link = link_metadata
        .as_ref()
        .is_ok_and(|metadata| metadata.file_type().is_symlink());
    let resolved = tokio::fs::canonicalize(&path).await;
    let (kind, state, size_bytes) = match resolved {
        Ok(resolved) if !resolved.starts_with(canonical_root) => (
            SessionResourceEntryKind::File,
            SessionResourceEntryState::OutsideRoot,
            None,
        ),
        Ok(resolved) => match tokio::fs::metadata(resolved).await {
            Ok(metadata) if metadata.is_dir() => (
                SessionResourceEntryKind::Directory,
                SessionResourceEntryState::Available,
                None,
            ),
            Ok(metadata) if metadata.is_file() => (
                SessionResourceEntryKind::File,
                SessionResourceEntryState::Available,
                Some(metadata.len()),
            ),
            _ => (
                SessionResourceEntryKind::File,
                SessionResourceEntryState::Unsupported,
                None,
            ),
        },
        Err(_) => (
            SessionResourceEntryKind::File,
            SessionResourceEntryState::Unsupported,
            None,
        ),
    };
    SessionResourceEntry {
        locator,
        display_name,
        kind,
        state,
        is_symbolic_link,
        is_hidden: hidden,
        is_generated: generated,
        size_bytes,
    }
}

fn compare_resource_entries(left: &SessionResourceEntry, right: &SessionResourceEntry) -> Ordering {
    let left_directory = left.kind == SessionResourceEntryKind::Directory;
    let right_directory = right.kind == SessionResourceEntryKind::Directory;
    right_directory
        .cmp(&left_directory)
        .then_with(|| {
            left.display_name
                .to_lowercase()
                .cmp(&right.display_name.to_lowercase())
        })
        .then_with(|| left.display_name.cmp(&right.display_name))
}

async fn read_session_resource_preview(
    path: &Path,
) -> Result<PreviewSessionResourceFileResult, RuntimeErrorInfo> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| resource_unavailable())?;
    if !metadata.is_file() {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "resource is not a regular file",
        ));
    }
    let media_type = crate::image::sniff_media_type(path).map_err(|_| resource_unavailable())?;
    let image = matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    );
    let pdf = media_type == "application/pdf";
    let limit = if image {
        MAX_IMAGE_PREVIEW_BYTES
    } else if pdf {
        MAX_PDF_PREVIEW_BYTES
    } else {
        MAX_TEXT_PREVIEW_BYTES
    };
    if metadata.len() > limit {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceTooLarge,
            "resource exceeds the preview size limit",
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| resource_unavailable())?;
    if image {
        let decoded = image::load_from_memory(&bytes).map_err(|_| {
            RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceNotPreviewable,
                "resource is not a supported image",
            )
        })?;
        if decoded.width() > MAX_IMAGE_EDGE
            || decoded.height() > MAX_IMAGE_EDGE
            || u64::from(decoded.width()) * u64::from(decoded.height()) > MAX_IMAGE_PIXELS
        {
            return Err(RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceTooLarge,
                "image dimensions exceed the preview limit",
            ));
        }
        return Ok(PreviewSessionResourceFileResult {
            kind: SessionResourcePreviewKind::Image,
            media_type,
            size_bytes: metadata.len(),
            text: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if pdf {
        return Ok(PreviewSessionResourceFileResult {
            kind: SessionResourcePreviewKind::Pdf,
            media_type,
            size_bytes: metadata.len(),
            text: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if bytes.contains(&0) {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "resource is not valid text",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "resource is not valid UTF-8 text",
        )
    })?;
    let media_type = preview_media_type(
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(""),
    )
    .filter(|value| value.starts_with("text/") || value.starts_with("application/json"))
    .unwrap_or("text/plain; charset=utf-8");
    Ok(PreviewSessionResourceFileResult {
        kind: SessionResourcePreviewKind::Text,
        media_type: media_type.to_owned(),
        size_bytes: metadata.len(),
        text: Some(text),
        data_base64: None,
    })
}

fn resource_unavailable() -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(
        RuntimeErrorCode::ResourceNotPreviewable,
        "resource is unavailable",
    )
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
        attachment.media_type.as_deref(),
        headers,
    )
    .await
}

/// 返回与原 Blob 相邻的固定 JPEG 缩略图；缺失时从原图按需恢复。
pub(super) async fn thumbnail_attachment(
    State(state): State<HttpState>,
    RoutePath((session_id, attachment_id)): RoutePath<(String, String)>,
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
    let source = std::path::PathBuf::from(attachment.agent_readable_path);
    let generated = tokio::task::spawn_blocking(move || crate::image::ensure_thumbnail(&source));
    let path = match generated.await {
        Ok(Ok(path)) => path,
        Ok(Err(crate::image::ImageResourceError::Unsupported)) => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceNotPreviewable,
                "attachment is not a supported image",
            ));
        }
        _ => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentUnavailable,
                "thumbnail is unavailable",
            ));
        }
    };
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return resource_error(RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentUnavailable,
                "thumbnail is unavailable",
            ));
        }
    };
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    response
}

pub(super) async fn preview_tool_file(
    State(state): State<HttpState>,
    RoutePath((session_id, message_id, resource_ref_id)): RoutePath<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        main_tool_resource_request(&session_id, &message_id, &resource_ref_id)
    else {
        return resource_error(invalid_request("tool resource identity is invalid"));
    };
    preview_tool_resource(state, owner, message_id, resource_ref_id, headers).await
}

pub(super) async fn preview_child_tool_file(
    State(state): State<HttpState>,
    RoutePath((session_id, child_task_id, message_id, resource_ref_id)): RoutePath<(
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        child_tool_resource_request(&session_id, &child_task_id, &message_id, &resource_ref_id)
    else {
        return resource_error(invalid_request("tool resource identity is invalid"));
    };
    preview_tool_resource(state, owner, message_id, resource_ref_id, headers).await
}

async fn preview_tool_resource(
    state: HttpState,
    owner: ConversationOwner,
    message_id: MessageId,
    resource_ref_id: ResourceRefId,
    headers: HeaderMap,
) -> Response {
    let resource = match state
        .runtime
        .resolve_tool_file_resource(&owner, &message_id, &resource_ref_id)
        .await
    {
        Ok(value) => value,
        Err(error) => return resource_error(error.to_protocol_info()),
    };
    if resource.origin == ToolFileResourceOrigin::SessionToolImage {
        let Some(reference) = resource.tool_image else {
            return resource_error(invalid_request("tool image reference is invalid"));
        };
        let Some(directory) = Path::new(&resource.path).parent().map(Path::to_path_buf) else {
            return resource_error(invalid_request("tool image path is invalid"));
        };
        let media_type = reference.media_type().to_owned();
        // Session Tool Image 已在写入时完成完整解码校验。预览只重验普通文件、MIME 与内容哈希，
        // 并复用本次读取的原始字节，避免重复解码、缩放、JPEG 编码和第二次文件读取。
        let loaded = tokio::task::spawn_blocking(move || {
            crate::image::read_tool_image_for_preview(&directory, &reference)
        });
        let bytes = match loaded.await {
            Ok(Ok(bytes)) if bytes.len() <= MAX_IMAGE_PREVIEW_BYTES as usize => bytes,
            Ok(Ok(_)) => {
                return resource_error(RuntimeErrorInfo::new(
                    RuntimeErrorCode::ResourceTooLarge,
                    "resource exceeds the preview size limit",
                ));
            }
            _ => {
                return resource_error(RuntimeErrorInfo::new(
                    RuntimeErrorCode::AttachmentUnavailable,
                    "tool image is unavailable",
                ));
            }
        };
        let mut response = Response::new(Body::from(bytes));
        if let Ok(value) = HeaderValue::from_str(&media_type) {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        );
        return response;
    }
    preview_path(
        Path::new(&resource.path),
        &resource.display_name,
        resource.media_type.as_deref(),
        headers,
    )
    .await
}

pub(super) async fn resolve_tool_file_native_path(
    State(state): State<HttpState>,
    RoutePath((session_id, message_id, resource_ref_id)): RoutePath<(String, String, String)>,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        main_tool_resource_request(&session_id, &message_id, &resource_ref_id)
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
    resolve_tool_native_path(resource).await
}

pub(super) async fn resolve_child_tool_file_native_path(
    State(state): State<HttpState>,
    RoutePath((session_id, child_task_id, message_id, resource_ref_id)): RoutePath<(
        String,
        String,
        String,
        String,
    )>,
) -> Response {
    let Some((owner, message_id, resource_ref_id)) =
        child_tool_resource_request(&session_id, &child_task_id, &message_id, &resource_ref_id)
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
    resolve_tool_native_path(resource).await
}

async fn resolve_tool_native_path(
    resource: assistant_runtime::ResolvedToolFileResource,
) -> Response {
    if resource.origin == ToolFileResourceOrigin::SessionToolImage {
        return resource_error(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "session tool images do not expose native paths",
        ));
    }
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

fn main_tool_resource_request(
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

fn child_tool_resource_request(
    session_id: &str,
    child_task_id: &str,
    message_id: &str,
    resource_ref_id: &str,
) -> Option<(ConversationOwner, MessageId, ResourceRefId)> {
    let session_id = SessionId::new(session_id.to_owned()).ok()?;
    Some((
        ConversationOwner::ChildTask {
            session_id,
            child_task_id: assistant_protocol::ChildTaskId::new(child_task_id.to_owned()).ok()?,
        },
        MessageId::new(message_id.to_owned()).ok()?,
        ResourceRefId::new(resource_ref_id.to_owned()).ok()?,
    ))
}

async fn preview_path(
    path: &Path,
    name: &str,
    actual_media_type: Option<&str>,
    headers: HeaderMap,
) -> Response {
    let media_type = match actual_media_type
        .and_then(previewable_media_type)
        .or_else(|| preview_media_type(name))
    {
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
    let max_size = if media_type.starts_with("image/") || media_type == "application/pdf" {
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

fn previewable_media_type(value: &str) -> Option<&'static str> {
    match value {
        "image/jpeg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "application/pdf" => Some("application/pdf"),
        "text/plain" => Some("text/plain; charset=utf-8"),
        "application/json" => Some("application/json; charset=utf-8"),
        _ => None,
    }
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
        "pdf" => Some("application/pdf"),
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
    use assistant_protocol::{ChildTaskId, SessionResourceRoot};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn child_tool_resource_identity_keeps_the_owner_namespace() {
        let (owner, message_id, resource_ref_id) =
            child_tool_resource_request("session-1", "child-1", "message-1", "resource-1")
                .expect("child resource identity");
        assert_eq!(message_id.as_str(), "message-1");
        assert_eq!(resource_ref_id.as_str(), "resource-1");
        assert_eq!(
            owner,
            ConversationOwner::ChildTask {
                session_id: SessionId::new("session-1").expect("session"),
                child_task_id: ChildTaskId::new("child-1").expect("child"),
            }
        );
    }

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

    #[test]
    fn relative_resource_paths_cannot_escape_the_root() {
        assert_eq!(
            normalize_relative_path("src/./main.rs").expect("relative path"),
            PathBuf::from("src/main.rs")
        );
        assert!(normalize_relative_path("../outside").is_err());
        assert!(normalize_relative_path("/absolute").is_err());
        assert!(normalize_relative_path("bad\0name").is_err());
    }

    #[tokio::test]
    async fn directory_entries_are_filtered_sorted_and_mark_outside_links() {
        let root = tempdir().expect("root");
        let outside = tempdir().expect("outside");
        tokio::fs::create_dir(root.path().join("src"))
            .await
            .expect("source directory");
        tokio::fs::create_dir(root.path().join("target"))
            .await
            .expect("generated directory");
        tokio::fs::write(root.path().join("README.md"), b"hello")
            .await
            .expect("text file");
        tokio::fs::write(root.path().join(".hidden"), b"secret")
            .await
            .expect("hidden file");
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape"))
            .expect("outside symlink");
        let canonical_root = tokio::fs::canonicalize(root.path())
            .await
            .expect("canonical root");
        let locator = SessionResourceLocator {
            root: SessionResourceRoot::WorkspacePrimary,
            relative_path: String::new(),
        };
        let result =
            read_directory_entries(&canonical_root, &canonical_root, &locator, false, false)
                .await
                .expect("directory listing");
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| entry.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "escape", "README.md"]
        );
        assert_eq!(
            result.entries[1].state,
            SessionResourceEntryState::OutsideRoot
        );
        assert!(result.entries[1].is_symbolic_link);
    }

    #[tokio::test]
    async fn text_preview_uses_content_validation_instead_of_requiring_an_extension() {
        let root = tempdir().expect("root");
        let text = root.path().join("notes.md");
        tokio::fs::write(&text, "你好".as_bytes())
            .await
            .expect("text file");
        let preview = read_session_resource_preview(&text)
            .await
            .expect("text preview");
        assert_eq!(preview.kind, SessionResourcePreviewKind::Text);
        assert_eq!(preview.text.as_deref(), Some("你好"));

        let binary = root.path().join("data.txt");
        tokio::fs::write(&binary, b"a\0b")
            .await
            .expect("binary file");
        assert!(read_session_resource_preview(&binary).await.is_err());

        let unknown = root.path().join("archive.bin");
        tokio::fs::write(&unknown, b"plain text")
            .await
            .expect("unknown file");
        let preview = read_session_resource_preview(&unknown)
            .await
            .expect("unknown text preview");
        assert_eq!(preview.kind, SessionResourcePreviewKind::Text);
        assert_eq!(preview.media_type, "text/plain; charset=utf-8");

        let extensionless = root.path().join("Jenkinsfile");
        tokio::fs::write(&extensionless, b"pipeline { agent any }")
            .await
            .expect("extensionless text file");
        let preview = read_session_resource_preview(&extensionless)
            .await
            .expect("extensionless text preview");
        assert_eq!(preview.kind, SessionResourcePreviewKind::Text);

        let pdf = root.path().join("document.data");
        let pdf_bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";
        tokio::fs::write(&pdf, pdf_bytes)
            .await
            .expect("PDF fixture");
        let preview = read_session_resource_preview(&pdf)
            .await
            .expect("PDF preview");
        assert_eq!(preview.kind, SessionResourcePreviewKind::Pdf);
        assert_eq!(preview.media_type, "application/pdf");
        assert_eq!(
            preview.data_base64.as_deref(),
            Some(STANDARD.encode(pdf_bytes).as_str())
        );

        let invalid_utf8 = root.path().join("invalid.data");
        tokio::fs::write(&invalid_utf8, [0xff, 0xfe, 0xfd])
            .await
            .expect("invalid UTF-8 file");
        assert!(read_session_resource_preview(&invalid_utf8).await.is_err());
    }

    #[tokio::test]
    async fn directory_listing_is_bounded_and_reports_truncation() {
        let root = tempdir().expect("root");
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            tokio::fs::write(root.path().join(format!("file-{index:04}.txt")), b"x")
                .await
                .expect("directory entry");
        }
        let canonical_root = tokio::fs::canonicalize(root.path())
            .await
            .expect("canonical root");
        let locator = SessionResourceLocator {
            root: SessionResourceRoot::WorkspacePrimary,
            relative_path: String::new(),
        };
        let result = read_directory_entries(&canonical_root, &canonical_root, &locator, true, true)
            .await
            .expect("directory listing");

        assert_eq!(result.entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(result.truncated);
        assert!(result.entries.windows(2).all(|entries| {
            compare_resource_entries(&entries[0], &entries[1]) != Ordering::Greater
        }));
    }
}
