//! 原生文件选择、流式附件上传、受控预览/打开与 Markdown 导出。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    AttachmentId, AttachmentState, GetAttachmentRequest, MessageId, ResourceRefId, RuntimeCommand,
    RuntimeCommandResult, RuntimeErrorInfo, SessionId, UploadAttachmentResult,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::io::AsyncWriteExt as _;
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::runtime_bootstrap::RuntimeBootstrapCoordinator;

const MAX_SELECTIONS: usize = 32;
const SELECTION_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_PREVIEW_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct NativeResourceBridge {
    selections: Mutex<HashMap<String, SelectedAttachment>>,
    operations: Mutex<HashMap<String, CancellationToken>>,
    next_id: AtomicU64,
    http: reqwest::Client,
}

#[derive(Clone)]
struct SelectedAttachment {
    path: PathBuf,
    original_name: String,
    size_bytes: u64,
    selected_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttachmentSelection {
    selection_id: String,
    original_name: String,
    size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreviewKind {
    Text,
    Image,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttachmentPreview {
    kind: PreviewKind,
    media_type: String,
    size_bytes: u64,
    text: Option<String>,
    data_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ExportResult {
    saved: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum NativeResourceErrorCode {
    InvalidRequest,
    SelectionUnavailable,
    SelectionLimitReached,
    AttachmentUnavailable,
    UploadFailed,
    PreviewUnavailable,
    ResourceNotPreviewable,
    ResourceTooLarge,
    SystemOpenFailed,
    ExportFailed,
    RuntimeUnavailable,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativeResourceError {
    code: NativeResourceErrorCode,
    message: String,
}

#[derive(Deserialize)]
struct RuntimeFailureBody {
    error: RuntimeErrorInfo,
}

#[derive(Deserialize)]
struct NativeResourcePath {
    path: String,
}

impl NativeResourceBridge {
    pub(crate) fn new() -> Self {
        Self {
            selections: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            http: reqwest::Client::new(),
        }
    }

    fn allocate_id(&self, prefix: &str) -> String {
        let next = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{}-{next}", std::process::id())
    }

    fn selected(&self, selection_id: &str) -> Result<SelectedAttachment, NativeResourceError> {
        let now = now_ms();
        let mut selections = self
            .selections
            .lock()
            .map_err(|_| unavailable("附件选择状态不可用。"))?;
        selections.retain(|_, item| {
            now.saturating_sub(item.selected_at_ms) <= SELECTION_TTL.as_millis() as u64
        });
        selections.get(selection_id).cloned().ok_or_else(|| {
            error(
                NativeResourceErrorCode::SelectionUnavailable,
                "附件选择已失效，请重新选择。",
            )
        })
    }

    fn register_operation(
        &self,
        operation_id: &str,
    ) -> Result<CancellationToken, NativeResourceError> {
        if operation_id.trim().is_empty() || operation_id.len() > 128 {
            return Err(error(
                NativeResourceErrorCode::InvalidRequest,
                "资源操作标识无效。",
            ));
        }
        let mut operations = self
            .operations
            .lock()
            .map_err(|_| unavailable("资源操作状态不可用。"))?;
        if operations.contains_key(operation_id) {
            return Err(error(
                NativeResourceErrorCode::InvalidRequest,
                "资源操作标识重复。",
            ));
        }
        let token = CancellationToken::new();
        operations.insert(operation_id.to_owned(), token.clone());
        Ok(token)
    }

    fn finish_operation(&self, operation_id: &str) {
        if let Ok(mut operations) = self.operations.lock() {
            operations.remove(operation_id);
        }
    }
}

#[tauri::command]
pub(crate) async fn choose_attachment_files(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
) -> Result<Vec<AttachmentSelection>, NativeResourceError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择附件")
        .pick_files(move |selection| {
            let _ = sender.send(selection);
        });
    let selections = receiver
        .await
        .map_err(|_| unavailable("无法打开附件选择器。"))?
        .unwrap_or_default();
    if selections.is_empty() {
        return Ok(Vec::new());
    }

    let mut prepared = Vec::with_capacity(selections.len());
    for selection in selections {
        let path = selection
            .into_path()
            .map_err(|_| unavailable("无法读取所选附件。"))?;
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| unavailable("无法读取所选附件。"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(error(
                NativeResourceErrorCode::AttachmentUnavailable,
                "只能选择可读取的普通文件。",
            ));
        }
        let original_name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| unavailable("附件名称不可用。"))?
            .to_owned();
        prepared.push((path, original_name, metadata.len()));
    }

    let now = now_ms();
    let mut state = bridge
        .selections
        .lock()
        .map_err(|_| unavailable("附件选择状态不可用。"))?;
    state.retain(|_, item| {
        now.saturating_sub(item.selected_at_ms) <= SELECTION_TTL.as_millis() as u64
    });
    if state.len().saturating_add(prepared.len()) > MAX_SELECTIONS {
        return Err(error(
            NativeResourceErrorCode::SelectionLimitReached,
            "一次最多保留 32 个待发送附件。",
        ));
    }
    let mut result = Vec::with_capacity(prepared.len());
    for (path, original_name, size_bytes) in prepared {
        let selection_id = bridge.allocate_id("selection");
        state.insert(
            selection_id.clone(),
            SelectedAttachment {
                path,
                original_name: original_name.clone(),
                size_bytes,
                selected_at_ms: now,
            },
        );
        result.push(AttachmentSelection {
            selection_id,
            original_name,
            size_bytes,
        });
    }
    Ok(result)
}

#[tauri::command]
pub(crate) fn release_attachment_selection(
    bridge: State<'_, NativeResourceBridge>,
    selection_id: String,
) -> Result<(), NativeResourceError> {
    bridge
        .selections
        .lock()
        .map_err(|_| unavailable("附件选择状态不可用。"))?
        .remove(&selection_id);
    Ok(())
}

#[tauri::command]
pub(crate) fn cancel_resource_operation(
    bridge: State<'_, NativeResourceBridge>,
    operation_id: String,
) -> Result<(), NativeResourceError> {
    if let Some(token) = bridge
        .operations
        .lock()
        .map_err(|_| unavailable("资源操作状态不可用。"))?
        .get(&operation_id)
        .cloned()
    {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn upload_selected_attachment(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    selection_id: String,
    operation_id: String,
) -> Result<UploadAttachmentResult, NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let selected = bridge.selected(&selection_id)?;
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    if let Some(maximum) = bootstrap.capabilities.max_attachment_bytes
        && selected.size_bytes > maximum
    {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "附件超过 Runtime 允许的单文件大小。",
        ));
    }
    let cancellation = bridge.register_operation(&operation_id)?;
    let result = upload_selected(
        &bridge.http,
        &bootstrap.base_url,
        &bootstrap.access_token,
        &session_id,
        &selected,
        cancellation,
    )
    .await;
    bridge.finish_operation(&operation_id);
    if result.is_ok()
        && let Ok(mut selections) = bridge.selections.lock()
    {
        selections.remove(&selection_id);
    }
    result
}

#[tauri::command]
pub(crate) async fn preview_attachment(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    attachment_id: String,
) -> Result<AttachmentPreview, NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let attachment_id = AttachmentId::new(attachment_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "附件标识无效。"))?;
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let response = bridge
        .http
        .get(format!(
            "{}/sessions/{}/attachments/{}/preview",
            bootstrap.base_url,
            session_id.as_str(),
            attachment_id.as_str()
        ))
        .bearer_auth(&bootstrap.access_token)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    decode_preview_response(response).await
}

#[tauri::command]
pub(crate) async fn preview_tool_file(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    message_id: String,
    resource_ref_id: String,
) -> Result<AttachmentPreview, NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let message_id = MessageId::new(message_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "消息标识无效。"))?;
    let resource_ref_id = ResourceRefId::new(resource_ref_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "文件引用无效。"))?;
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let url = runtime_resource_url(
        &bootstrap.base_url,
        &[
            "sessions",
            session_id.as_str(),
            "messages",
            message_id.as_str(),
            "resources",
            resource_ref_id.as_str(),
            "preview",
        ],
    )?;
    let response = bridge
        .http
        .get(url)
        .bearer_auth(&bootstrap.access_token)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    decode_preview_response(response).await
}

async fn decode_preview_response(
    response: reqwest::Response,
) -> Result<AttachmentPreview, NativeResourceError> {
    if !response.status().is_success() {
        return Err(
            decode_runtime_failure(response, NativeResourceErrorCode::PreviewUnavailable).await,
        );
    }
    let media_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let bytes = response.bytes().await.map_err(|_| {
        error(
            NativeResourceErrorCode::PreviewUnavailable,
            "无法读取附件预览。",
        )
    })?;
    if bytes.len() > MAX_PREVIEW_RESPONSE_BYTES {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "附件预览超过大小限制。",
        ));
    }
    let size_bytes = bytes.len() as u64;
    if media_type.starts_with("image/") {
        let data_url = format!("data:{media_type};base64,{}", STANDARD.encode(&bytes));
        return Ok(AttachmentPreview {
            kind: PreviewKind::Image,
            media_type,
            size_bytes,
            text: None,
            data_url: Some(data_url),
        });
    }
    if media_type.starts_with("text/") || media_type.starts_with("application/json") {
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            error(
                NativeResourceErrorCode::ResourceNotPreviewable,
                "文件不是有效的 UTF-8 文本。",
            )
        })?;
        return Ok(AttachmentPreview {
            kind: PreviewKind::Text,
            media_type,
            size_bytes,
            text: Some(text),
            data_url: None,
        });
    }
    Err(error(
        NativeResourceErrorCode::ResourceNotPreviewable,
        "该文件类型不支持应用内预览。",
    ))
}

#[tauri::command]
pub(crate) async fn open_attachment_in_system(
    app: AppHandle,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    attachment_id: String,
) -> Result<(), NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let attachment_id = AttachmentId::new(attachment_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "附件标识无效。"))?;
    let attachment = get_attachment(&coordinator, session_id, attachment_id).await?;
    if attachment.state != AttachmentState::Ready {
        return Err(error(
            NativeResourceErrorCode::AttachmentUnavailable,
            "附件当前不可用。",
        ));
    }
    app.opener()
        .open_path(attachment.agent_readable_path, None::<&str>)
        .map_err(|_| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法使用系统应用打开附件。",
            )
        })
}

#[tauri::command]
pub(crate) async fn reveal_attachment_in_directory(
    app: AppHandle,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    attachment_id: String,
) -> Result<(), NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let attachment_id = AttachmentId::new(attachment_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "附件标识无效。"))?;
    let attachment = get_attachment(&coordinator, session_id, attachment_id).await?;
    if attachment.state != AttachmentState::Ready {
        return Err(error(
            NativeResourceErrorCode::AttachmentUnavailable,
            "附件当前不可用。",
        ));
    }
    app.opener()
        .reveal_item_in_dir(attachment.agent_readable_path)
        .map_err(|_| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法在目录中显示附件。",
            )
        })
}

#[tauri::command]
pub(crate) async fn open_tool_file_in_system(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    message_id: String,
    resource_ref_id: String,
) -> Result<(), NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let message_id = MessageId::new(message_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "消息标识无效。"))?;
    let resource_ref_id = ResourceRefId::new(resource_ref_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "文件引用无效。"))?;
    let resource = get_tool_file_native_path(
        &bridge,
        &coordinator,
        &session_id,
        &message_id,
        &resource_ref_id,
    )
    .await?;
    app.opener()
        .open_path(resource.path, None::<&str>)
        .map_err(|_| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法使用系统应用打开文件。",
            )
        })
}

#[tauri::command]
pub(crate) async fn reveal_tool_file_in_directory(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    message_id: String,
    resource_ref_id: String,
) -> Result<(), NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let message_id = MessageId::new(message_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "消息标识无效。"))?;
    let resource_ref_id = ResourceRefId::new(resource_ref_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "文件引用无效。"))?;
    let resource = get_tool_file_native_path(
        &bridge,
        &coordinator,
        &session_id,
        &message_id,
        &resource_ref_id,
    )
    .await?;
    app.opener().reveal_item_in_dir(resource.path).map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法在目录中显示文件。",
        )
    })
}

async fn get_tool_file_native_path(
    bridge: &NativeResourceBridge,
    coordinator: &RuntimeBootstrapCoordinator,
    session_id: &SessionId,
    message_id: &MessageId,
    resource_ref_id: &ResourceRefId,
) -> Result<NativeResourcePath, NativeResourceError> {
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let url = runtime_resource_url(
        &bootstrap.base_url,
        &[
            "sessions",
            session_id.as_str(),
            "messages",
            message_id.as_str(),
            "resources",
            resource_ref_id.as_str(),
            "native-path",
        ],
    )?;
    let response = bridge
        .http
        .get(url)
        .bearer_auth(&bootstrap.access_token)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    if !response.status().is_success() {
        return Err(
            decode_runtime_failure(response, NativeResourceErrorCode::SystemOpenFailed).await,
        );
    }
    response.json::<NativeResourcePath>().await.map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法解析文件位置。",
        )
    })
}

#[tauri::command]
pub(crate) async fn export_session_markdown(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    suggested_name: String,
) -> Result<ExportResult, NativeResourceError> {
    let session_id = SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))?;
    let file_name = export_file_name(&suggested_name);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导出会话 Markdown")
        .set_file_name(&file_name)
        .add_filter("Markdown", &["md"])
        .save_file(move |selection| {
            let _ = sender.send(selection);
        });
    let Some(selection) = receiver.await.map_err(|_| {
        error(
            NativeResourceErrorCode::ExportFailed,
            "无法打开保存对话框。",
        )
    })?
    else {
        return Ok(ExportResult { saved: false });
    };
    let target = selection
        .into_path()
        .map_err(|_| error(NativeResourceErrorCode::ExportFailed, "导出目标不可用。"))?;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| error(NativeResourceErrorCode::ExportFailed, "导出目标不可用。"))?;
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let mut response = bridge
        .http
        .get(format!(
            "{}/sessions/{}/export.md",
            bootstrap.base_url,
            session_id.as_str()
        ))
        .bearer_auth(&bootstrap.access_token)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    if !response.status().is_success() {
        return Err(decode_runtime_failure(response, NativeResourceErrorCode::ExportFailed).await);
    }
    let temporary = temporary_export_path(parent, &bridge.allocate_id("export"));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|_| {
            error(
                NativeResourceErrorCode::ExportFailed,
                "无法创建导出临时文件。",
            )
        })?;
    let write_result = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error(NativeResourceErrorCode::ExportFailed, "导出内容下载失败。"))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|_| error(NativeResourceErrorCode::ExportFailed, "导出文件写入失败。"))?;
        }
        file.flush()
            .await
            .map_err(|_| error(NativeResourceErrorCode::ExportFailed, "导出文件写入失败。"))?;
        file.sync_all()
            .await
            .map_err(|_| error(NativeResourceErrorCode::ExportFailed, "导出文件写入失败。"))?;
        drop(file);
        tokio::fs::rename(&temporary, &target).await.map_err(|_| {
            error(
                NativeResourceErrorCode::ExportFailed,
                "无法原子替换导出目标。",
            )
        })?;
        Ok::<(), NativeResourceError>(())
    }
    .await;
    if write_result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    write_result?;
    Ok(ExportResult { saved: true })
}

async fn upload_selected(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    session_id: &SessionId,
    selected: &SelectedAttachment,
    cancellation: CancellationToken,
) -> Result<UploadAttachmentResult, NativeResourceError> {
    let file = tokio::fs::File::open(&selected.path).await.map_err(|_| {
        error(
            NativeResourceErrorCode::SelectionUnavailable,
            "无法读取所选附件。",
        )
    })?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let part = Part::stream_with_length(body, selected.size_bytes)
        .file_name(selected.original_name.clone());
    let request = http
        .post(format!(
            "{base_url}/sessions/{}/attachments",
            session_id.as_str()
        ))
        .bearer_auth(access_token)
        .multipart(Form::new().part("file", part))
        .send();
    let response = tokio::select! {
        () = cancellation.cancelled() => {
            return Err(error(NativeResourceErrorCode::Cancelled, "附件上传已取消。"));
        }
        response = request => response.map_err(|_| runtime_unavailable())?,
    };
    if !response.status().is_success() {
        return Err(decode_runtime_failure(response, NativeResourceErrorCode::UploadFailed).await);
    }
    response
        .json::<UploadAttachmentResult>()
        .await
        .map_err(|_| {
            error(
                NativeResourceErrorCode::UploadFailed,
                "Runtime 返回了无效的附件结果。",
            )
        })
}

async fn get_attachment(
    coordinator: &RuntimeBootstrapCoordinator,
    session_id: SessionId,
    attachment_id: AttachmentId,
) -> Result<assistant_protocol::AttachmentSummary, NativeResourceError> {
    #[derive(Serialize)]
    struct CommandRequest {
        request_id: String,
        command: CommandScope,
    }
    #[derive(Serialize)]
    #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
    enum CommandScope {
        Runtime(RuntimeCommand),
    }
    #[derive(Deserialize)]
    struct CommandResponse {
        result: ResultScope,
    }
    #[derive(Deserialize)]
    #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
    enum ResultScope {
        Runtime(RuntimeCommandResult),
    }

    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let response = reqwest::Client::new()
        .post(format!("{}/commands", bootstrap.base_url))
        .bearer_auth(&bootstrap.access_token)
        .json(&CommandRequest {
            request_id: "desktop-open-attachment".to_owned(),
            command: CommandScope::Runtime(RuntimeCommand::GetAttachment(GetAttachmentRequest {
                session_id,
                attachment_id,
            })),
        })
        .send()
        .await
        .map_err(|_| runtime_unavailable())?
        .error_for_status()
        .map_err(|_| runtime_unavailable())?
        .json::<CommandResponse>()
        .await
        .map_err(|_| runtime_unavailable())?;
    match response.result {
        ResultScope::Runtime(RuntimeCommandResult::GetAttachment(result)) => Ok(result.attachment),
        _ => Err(runtime_unavailable()),
    }
}

async fn decode_runtime_failure(
    response: reqwest::Response,
    fallback: NativeResourceErrorCode,
) -> NativeResourceError {
    if let Ok(body) = response.json::<RuntimeFailureBody>().await {
        let code = match body.error.code {
            assistant_protocol::RuntimeErrorCode::ResourceNotPreviewable => {
                NativeResourceErrorCode::ResourceNotPreviewable
            }
            assistant_protocol::RuntimeErrorCode::ResourceTooLarge
            | assistant_protocol::RuntimeErrorCode::AttachmentTooLarge => {
                NativeResourceErrorCode::ResourceTooLarge
            }
            assistant_protocol::RuntimeErrorCode::AttachmentNotFound
            | assistant_protocol::RuntimeErrorCode::AttachmentUnavailable => {
                NativeResourceErrorCode::AttachmentUnavailable
            }
            _ => fallback,
        };
        return error(code, body.error.message);
    }
    error(fallback, "Runtime 资源请求失败。")
}

fn export_file_name(value: &str) -> String {
    let stem = value
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            value if value.is_control() => '_',
            value => value,
        })
        .collect::<String>();
    let stem = stem.trim().trim_end_matches('.');
    let stem = if stem.is_empty() { "会话" } else { stem };
    format!("{stem}.md")
}

fn temporary_export_path(parent: &Path, operation_id: &str) -> PathBuf {
    parent.join(format!(".ez-assistant-{operation_id}.tmp"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn runtime_resource_url(
    base_url: &str,
    segments: &[&str],
) -> Result<reqwest::Url, NativeResourceError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| runtime_unavailable())?;
    url.path_segments_mut()
        .map_err(|_| runtime_unavailable())?
        .pop_if_empty()
        .extend(segments.iter().copied());
    Ok(url)
}

fn runtime_unavailable() -> NativeResourceError {
    error(
        NativeResourceErrorCode::RuntimeUnavailable,
        "无法连接本地 Runtime。",
    )
}

fn unavailable(message: &'static str) -> NativeResourceError {
    error(NativeResourceErrorCode::AttachmentUnavailable, message)
}

fn error(code: NativeResourceErrorCode, message: impl Into<String>) -> NativeResourceError {
    NativeResourceError {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_name_never_contains_path_components() {
        assert_eq!(export_file_name("a/b:c"), "a_b_c.md");
        assert_eq!(export_file_name("  "), "会话.md");
    }

    #[test]
    fn temporary_export_stays_in_selected_directory() {
        let parent = Path::new("/tmp/export-target");
        let path = temporary_export_path(parent, "export-1");
        assert_eq!(path.parent(), Some(parent));
    }
}
