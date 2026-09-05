//! 原生文件选择、流式附件上传、受控预览/打开与 Markdown 导出。

use std::{
    collections::HashMap,
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    AttachmentId, AttachmentState, ChildTaskId, GetAttachmentRequest,
    ListSessionResourceFilesRequest, ListSessionResourceFilesResult, MessageId,
    PreviewSessionResourceFileRequest, PreviewSessionResourceFileResult, ResourceRefId,
    RuntimeCommand, RuntimeCommandResult, RuntimeErrorInfo, RuntimeHostFeature, SessionId,
    SessionMaterializationManifest, SessionMaterializationResult, SessionResourceLocator,
    UploadAttachmentResult,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::GenericImageView as _;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, ipc::InvokeBody};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::runtime_bootstrap::RuntimeBootstrapCoordinator;

const MAX_SELECTIONS: usize = 256;
const MAX_SELECTIONS_PER_SEND: usize = 32;
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CLIPBOARD_TEMP_BYTES: u64 = 256 * 1024 * 1024;
const SELECTION_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_PREVIEW_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEXT_PREVIEW_BYTES: u64 = 4 * 1024 * 1024;
const MAX_IMAGE_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_RESOURCE_HANDLES: usize = 512;

pub(crate) struct NativeResourceBridge {
    selections: Mutex<HashMap<String, SelectedAttachment>>,
    operations: Mutex<HashMap<String, CancellationToken>>,
    resource_handles: Mutex<ResourceHandleRegistry>,
    next_id: AtomicU64,
    http: reqwest::Client,
}

#[derive(Default)]
struct ResourceHandleRegistry {
    entries: HashMap<String, LocalResourceHandle>,
    keys_by_path: HashMap<PathBuf, String>,
}

#[derive(Clone)]
struct LocalResourceHandle {
    path: PathBuf,
    navigation_root: PathBuf,
}

#[derive(Clone)]
struct SelectedAttachment {
    source: SelectedAttachmentSource,
    original_name: String,
    size_bytes: u64,
    media_type: Option<String>,
    selected_at_ms: u64,
}

#[derive(Clone)]
enum SelectedAttachmentSource {
    ExternalPath(PathBuf),
    OwnedTempFile(Arc<File>),
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttachmentSelection {
    selection_id: String,
    original_name: String,
    size_bytes: u64,
    media_type: Option<String>,
    origin: AttachmentSelectionOrigin,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum AttachmentSelectionOrigin {
    FilePicker,
    Clipboard,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreviewKind {
    Text,
    Image,
    Pdf,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AttachmentPreview {
    kind: PreviewKind,
    media_type: String,
    size_bytes: u64,
    text: Option<String>,
    data_url: Option<String>,
    data_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RegisteredLocalResource {
    resource_key: String,
    display_name: String,
    path_segments: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LocalResourcePreview {
    kind: PreviewKind,
    media_type: String,
    size_bytes: u64,
    text: Option<String>,
    data_base64: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalResourceSiblingKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LocalResourceSibling {
    display_name: String,
    kind: LocalResourceSiblingKind,
    current: bool,
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
    MaterializationUnsupported,
    MaterializationFailed,
    MaterializationResponseUnknown,
    PreviewUnavailable,
    ResourceNotPreviewable,
    ResourceTooLarge,
    SystemOpenFailed,
    ExportFailed,
    RuntimeUnavailable,
    ResourceOutsideRoot,
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
            resource_handles: Mutex::new(ResourceHandleRegistry::default()),
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

    fn register_local_path(
        &self,
        path: PathBuf,
        navigation_root: PathBuf,
    ) -> Result<RegisteredLocalResource, NativeResourceError> {
        let display_name = display_file_name(&path)?;
        let path_segments = display_path_segments(&path)?;
        let mut registry = self
            .resource_handles
            .lock()
            .map_err(|_| unavailable("本地资源状态不可用。"))?;
        if let Some(resource_key) = registry.keys_by_path.get(&path).cloned() {
            if let Some(entry) = registry.entries.get_mut(&resource_key)
                && navigation_root.components().count() > entry.navigation_root.components().count()
            {
                entry.navigation_root = navigation_root;
            }
            return Ok(RegisteredLocalResource {
                resource_key,
                display_name,
                path_segments,
            });
        }
        if registry.entries.len() >= MAX_RESOURCE_HANDLES {
            return Err(error(
                NativeResourceErrorCode::SelectionLimitReached,
                "本次应用运行登记的本地资源过多，请重启应用后再试。",
            ));
        }
        let resource_key = self.allocate_id("resource");
        registry
            .keys_by_path
            .insert(path.clone(), resource_key.clone());
        registry.entries.insert(
            resource_key.clone(),
            LocalResourceHandle {
                path,
                navigation_root,
            },
        );
        Ok(RegisteredLocalResource {
            resource_key,
            display_name,
            path_segments,
        })
    }

    fn local_resource(
        &self,
        resource_key: &str,
    ) -> Result<LocalResourceHandle, NativeResourceError> {
        self.resource_handles
            .lock()
            .map_err(|_| unavailable("本地资源状态不可用。"))?
            .entries
            .get(resource_key)
            .cloned()
            .ok_or_else(|| {
                error(
                    NativeResourceErrorCode::SelectionUnavailable,
                    "本地资源句柄已失效，请重新打开链接。",
                )
            })
    }
}

#[tauri::command]
pub(crate) async fn register_local_file_uri(
    bridge: State<'_, NativeResourceBridge>,
    file_uri: String,
) -> Result<RegisteredLocalResource, NativeResourceError> {
    let path = file_uri_path(&file_uri)?;
    let canonical = validate_local_file(path).await?;
    let navigation_root = canonical
        .parent()
        .ok_or_else(invalid_local_resource)?
        .to_path_buf();
    bridge.register_local_path(canonical, navigation_root)
}

#[tauri::command]
pub(crate) async fn register_relative_local_resource(
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
    reference: String,
) -> Result<RegisteredLocalResource, NativeResourceError> {
    if reference.trim().is_empty() || reference.contains('\0') || reference.contains('\\') {
        return Err(invalid_local_resource());
    }
    let source = bridge.local_resource(&resource_key)?;
    let reference_path = relative_reference_path(&reference)?;
    let parent = source.path.parent().ok_or_else(invalid_local_resource)?;
    let canonical = validate_local_file(parent.join(reference_path)).await?;
    if !canonical.starts_with(&source.navigation_root) {
        return Err(error(
            NativeResourceErrorCode::ResourceOutsideRoot,
            "本地资源位于当前文件范围之外。",
        ));
    }
    bridge.register_local_path(canonical, source.navigation_root)
}

#[tauri::command]
pub(crate) async fn preview_local_resource(
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
) -> Result<LocalResourcePreview, NativeResourceError> {
    let resource = bridge.local_resource(&resource_key)?;
    let canonical = validate_registered_resource(&resource).await?;
    preview_local_path(&canonical).await
}

#[tauri::command]
pub(crate) async fn list_local_resource_siblings(
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
) -> Result<Vec<LocalResourceSibling>, NativeResourceError> {
    let resource = bridge.local_resource(&resource_key)?;
    let canonical = validate_registered_resource(&resource).await?;
    let mut directory = tokio::fs::read_dir(&resource.navigation_root)
        .await
        .map_err(|_| unavailable("无法读取本地资源目录。"))?;
    let mut siblings = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|_| unavailable("无法读取本地资源目录。"))?
    {
        if siblings.len() >= 2_000 {
            break;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| unavailable("资源名称不是有效文本。"))?;
        let file_type = entry
            .file_type()
            .await
            .map_err(|_| unavailable("无法读取本地资源信息。"))?;
        let kind = if file_type.is_dir() {
            LocalResourceSiblingKind::Directory
        } else if file_type.is_file() {
            LocalResourceSiblingKind::File
        } else {
            continue;
        };
        siblings.push(LocalResourceSibling {
            display_name: name,
            kind,
            current: entry.path() == canonical,
        });
    }
    siblings.sort_by(|left, right| {
        sibling_order(left.kind)
            .cmp(&sibling_order(right.kind))
            .then_with(|| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
            })
    });
    Ok(siblings)
}

#[tauri::command]
pub(crate) async fn register_local_resource_sibling(
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
    display_name: String,
) -> Result<RegisteredLocalResource, NativeResourceError> {
    if display_name.is_empty()
        || display_name.contains('/')
        || display_name.contains('\\')
        || display_name == "."
        || display_name == ".."
    {
        return Err(invalid_local_resource());
    }
    let source = bridge.local_resource(&resource_key)?;
    let canonical = validate_local_file(source.navigation_root.join(display_name)).await?;
    if canonical.parent() != Some(source.navigation_root.as_path()) {
        return Err(error(
            NativeResourceErrorCode::ResourceOutsideRoot,
            "本地资源位于当前文件范围之外。",
        ));
    }
    bridge.register_local_path(canonical, source.navigation_root)
}

#[tauri::command]
pub(crate) async fn open_local_resource_in_system(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
) -> Result<(), NativeResourceError> {
    let resource = bridge.local_resource(&resource_key)?;
    let canonical = validate_registered_resource(&resource).await?;
    let path = canonical
        .to_str()
        .ok_or_else(|| unavailable("本地资源路径不是有效文本。"))?;
    app.opener().open_path(path, None::<&str>).map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法使用系统应用打开该资源。",
        )
    })
}

#[tauri::command]
pub(crate) async fn reveal_local_resource_in_directory(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
) -> Result<(), NativeResourceError> {
    let resource = bridge.local_resource(&resource_key)?;
    let canonical = validate_registered_resource(&resource).await?;
    app.opener().reveal_item_in_dir(canonical).map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法在 Finder 中显示该资源。",
        )
    })
}

#[tauri::command]
pub(crate) async fn copy_local_resource_path(
    bridge: State<'_, NativeResourceBridge>,
    resource_key: String,
) -> Result<(), NativeResourceError> {
    let resource = bridge.local_resource(&resource_key)?;
    let canonical = validate_registered_resource(&resource).await?;
    let path = canonical
        .to_str()
        .ok_or_else(|| unavailable("本地资源路径不是有效文本。"))?;
    copy_path_to_clipboard(path)
}

#[tauri::command]
pub(crate) async fn list_session_resource_files(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    request: ListSessionResourceFilesRequest,
) -> Result<ListSessionResourceFilesResult, NativeResourceError> {
    let session_id = parse_session_id(session_id)?;
    send_session_resource_request(&bridge.http, &coordinator, &session_id, "list", &request).await
}

#[tauri::command]
pub(crate) async fn preview_session_resource_file(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    request: PreviewSessionResourceFileRequest,
) -> Result<PreviewSessionResourceFileResult, NativeResourceError> {
    let session_id = parse_session_id(session_id)?;
    send_session_resource_request(&bridge.http, &coordinator, &session_id, "preview", &request)
        .await
}

#[tauri::command]
pub(crate) async fn open_session_resource_in_system(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    locator: SessionResourceLocator,
) -> Result<(), NativeResourceError> {
    let path = resolve_session_resource_path(&bridge, &coordinator, session_id, locator).await?;
    app.opener().open_path(path, None::<&str>).map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法使用系统应用打开该资源。",
        )
    })
}

#[tauri::command]
pub(crate) async fn copy_session_resource_path(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    locator: SessionResourceLocator,
) -> Result<(), NativeResourceError> {
    let path = resolve_session_resource_path(&bridge, &coordinator, session_id, locator).await?;
    copy_path_to_clipboard(&path)
}

fn copy_path_to_clipboard(path: &str) -> Result<(), NativeResourceError> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法访问系统剪贴板。",
            )
        })?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法访问系统剪贴板。",
            )
        })?
        .write_all(path.as_bytes())
        .map_err(|_| {
            error(
                NativeResourceErrorCode::SystemOpenFailed,
                "无法复制资源路径。",
            )
        })?;
    let status = child.wait().map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法复制资源路径。",
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法复制资源路径。",
        ))
    }
}

#[tauri::command]
pub(crate) async fn reveal_session_resource_in_directory(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    locator: SessionResourceLocator,
) -> Result<(), NativeResourceError> {
    let path = resolve_session_resource_path(&bridge, &coordinator, session_id, locator).await?;
    app.opener().reveal_item_in_dir(path).map_err(|_| {
        error(
            NativeResourceErrorCode::SystemOpenFailed,
            "无法在 Finder 中显示该资源。",
        )
    })
}

fn parse_session_id(session_id: String) -> Result<SessionId, NativeResourceError> {
    SessionId::new(session_id)
        .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "会话标识无效。"))
}

pub(crate) async fn resolve_session_resource_path(
    bridge: &NativeResourceBridge,
    coordinator: &RuntimeBootstrapCoordinator,
    session_id: String,
    locator: SessionResourceLocator,
) -> Result<String, NativeResourceError> {
    let session_id = parse_session_id(session_id)?;
    let resource: NativeResourcePath = send_session_resource_request(
        &bridge.http,
        coordinator,
        &session_id,
        "native-path",
        &locator,
    )
    .await?;
    Ok(resource.path)
}

async fn send_session_resource_request<Request, ResponseBody>(
    http: &reqwest::Client,
    coordinator: &RuntimeBootstrapCoordinator,
    session_id: &SessionId,
    operation: &str,
    request: &Request,
) -> Result<ResponseBody, NativeResourceError>
where
    Request: Serialize + ?Sized,
    ResponseBody: for<'de> Deserialize<'de>,
{
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let url = runtime_resource_url(
        &bootstrap.base_url,
        &["sessions", session_id.as_str(), "resource-files", operation],
    )?;
    let response = http
        .post(url)
        .bearer_auth(&bootstrap.access_token)
        .json(request)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    if !response.status().is_success() {
        return Err(
            decode_runtime_failure(response, NativeResourceErrorCode::PreviewUnavailable).await,
        );
    }
    response
        .json::<ResponseBody>()
        .await
        .map_err(|_| unavailable("Runtime 资源响应无效。"))
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
    if selections.len() > MAX_SELECTIONS_PER_SEND {
        return Err(error(
            NativeResourceErrorCode::SelectionLimitReached,
            "一次最多选择 32 个待发送附件。",
        ));
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
            "当前草稿保留的附件过多，请先发送或移除部分附件。",
        ));
    }
    let mut result = Vec::with_capacity(prepared.len());
    for (path, original_name, size_bytes) in prepared {
        let selection_id = bridge.allocate_id("selection");
        state.insert(
            selection_id.clone(),
            SelectedAttachment {
                source: SelectedAttachmentSource::ExternalPath(path),
                original_name: original_name.clone(),
                size_bytes,
                media_type: None,
                selected_at_ms: now,
            },
        );
        result.push(AttachmentSelection {
            selection_id,
            original_name,
            size_bytes,
            media_type: None,
            origin: AttachmentSelectionOrigin::FilePicker,
        });
    }
    Ok(result)
}

/// 将一次 Composer paste 中的原始图片字节登记到现有 selection 池。
///
/// raw body 与展示元数据只存在于 Tauri IPC；真实媒体类型由 magic bytes 决定，Web MIME 只用于
/// 提前拒绝明显不合法的调用。匿名临时文件的最后一个句柄随 selection 释放而回收。
#[tauri::command]
pub(crate) fn stage_clipboard_image(
    bridge: State<'_, NativeResourceBridge>,
    request: tauri::ipc::Request<'_>,
) -> Result<AttachmentSelection, NativeResourceError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(error(
            NativeResourceErrorCode::InvalidRequest,
            "剪贴板图片必须使用原始字节传输。",
        ));
    };
    let media_type_hint = request
        .headers()
        .get("x-ez-media-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let original_name = request
        .headers()
        .get("x-ez-original-name")
        .and_then(|value| value.to_str().ok())
        .and_then(decode_header_value);
    stage_clipboard_image_bytes(&bridge, bytes, media_type_hint, original_name.as_deref())
}

fn stage_clipboard_image_bytes(
    bridge: &NativeResourceBridge,
    bytes: &[u8],
    media_type_hint: &str,
    original_name: Option<&str>,
) -> Result<AttachmentSelection, NativeResourceError> {
    if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "剪贴板图片为空或超过 32 MiB 限制。",
        ));
    }
    if !media_type_hint.to_ascii_lowercase().starts_with("image/") {
        return Err(error(
            NativeResourceErrorCode::InvalidRequest,
            "剪贴板内容没有声明为图片。",
        ));
    }
    let kind = infer::get(bytes).filter(|kind| kind.mime_type().starts_with("image/"));
    let kind = kind.ok_or_else(|| {
        error(
            NativeResourceErrorCode::InvalidRequest,
            "剪贴板内容不是受支持的图片文件。",
        )
    })?;
    let mut file = tempfile::tempfile().map_err(|_| unavailable("无法暂存剪贴板图片。"))?;
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .map_err(|_| unavailable("无法暂存剪贴板图片。"))?;

    let now = now_ms();
    let mut state = bridge
        .selections
        .lock()
        .map_err(|_| unavailable("附件选择状态不可用。"))?;
    state.retain(|_, item| {
        now.saturating_sub(item.selected_at_ms) <= SELECTION_TTL.as_millis() as u64
    });
    if state.len() >= MAX_SELECTIONS {
        return Err(error(
            NativeResourceErrorCode::SelectionLimitReached,
            "当前草稿保留的附件过多，请先发送或移除部分附件。",
        ));
    }
    let owned_bytes = state
        .values()
        .filter(|item| matches!(&item.source, SelectedAttachmentSource::OwnedTempFile(_)))
        .map(|item| item.size_bytes)
        .sum::<u64>();
    if owned_bytes.saturating_add(bytes.len() as u64) > MAX_CLIPBOARD_TEMP_BYTES {
        return Err(error(
            NativeResourceErrorCode::SelectionLimitReached,
            "剪贴板图片暂存总量已达到 256 MiB 限制。",
        ));
    }

    let selection_id = bridge.allocate_id("selection");
    let fallback_name = format!(
        "clipboard-image-{}.{}",
        selection_id.rsplit('-').next().unwrap_or("1"),
        kind.extension()
    );
    let original_name = safe_original_name(original_name).unwrap_or(fallback_name);
    let media_type = kind.mime_type().to_owned();
    state.insert(
        selection_id.clone(),
        SelectedAttachment {
            source: SelectedAttachmentSource::OwnedTempFile(Arc::new(file)),
            original_name: original_name.clone(),
            size_bytes: bytes.len() as u64,
            media_type: Some(media_type.clone()),
            selected_at_ms: now,
        },
    );
    Ok(AttachmentSelection {
        selection_id,
        original_name,
        size_bytes: bytes.len() as u64,
        media_type: Some(media_type),
        origin: AttachmentSelectionOrigin::Clipboard,
    })
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

/// 把同一草稿的原生 selection 作为一个 multipart 请求发送到首次物化入口。
///
/// selection 在明确失败时继续保留；只有收到可靠成功响应后才统一移除。请求发送阶段断连时无法判断
/// Runtime 是否已经提交，因此返回独立错误码，让 Desktop 使用原 manifest 与幂等键重试。
#[tauri::command]
pub(crate) async fn materialize_new_session(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    manifest: SessionMaterializationManifest,
    operation_id: String,
) -> Result<SessionMaterializationResult, NativeResourceError> {
    if manifest.attachments.len() > MAX_SELECTIONS_PER_SEND {
        return Err(error(
            NativeResourceErrorCode::SelectionLimitReached,
            "一次最多发送 32 个附件。",
        ));
    }
    let mut selected = Vec::with_capacity(manifest.attachments.len());
    for declared in &manifest.attachments {
        let attachment = bridge.selected(&declared.selection_key)?;
        if attachment.original_name != declared.original_name
            || attachment.size_bytes != declared.size_bytes
        {
            return Err(error(
                NativeResourceErrorCode::InvalidRequest,
                "附件选择与首次发送内容不一致。",
            ));
        }
        selected.push((declared.selection_key.clone(), attachment));
    }
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    if !bootstrap
        .capabilities
        .features
        .contains(&RuntimeHostFeature::SessionMaterialization)
    {
        return Err(error(
            NativeResourceErrorCode::MaterializationUnsupported,
            "当前 Runtime 不支持新会话首次发送，请重启或完成应用更新。",
        ));
    }
    if let Some(maximum) = bootstrap.capabilities.max_attachment_bytes
        && selected.iter().any(|(_, item)| item.size_bytes > maximum)
    {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "附件超过 Runtime 允许的单文件大小。",
        ));
    }
    let cancellation = bridge.register_operation(&operation_id)?;
    let result = materialize_selected(
        &bridge.http,
        &bootstrap.base_url,
        &bootstrap.access_token,
        &manifest,
        &selected,
        cancellation,
    )
    .await;
    bridge.finish_operation(&operation_id);
    if result.is_ok()
        && let Ok(mut selections) = bridge.selections.lock()
    {
        for (selection_id, _) in &selected {
            selections.remove(selection_id);
        }
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
pub(crate) async fn preview_attachment_selection(
    bridge: State<'_, NativeResourceBridge>,
    selection_id: String,
) -> Result<AttachmentPreview, NativeResourceError> {
    let selected = bridge.selected(&selection_id)?;
    if selected.size_bytes > MAX_PREVIEW_RESPONSE_BYTES as u64 {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "附件预览超过大小限制。",
        ));
    }
    let bytes = read_selected_bytes(&selected, Some(MAX_PREVIEW_RESPONSE_BYTES)).await?;
    let media_type = selected
        .media_type
        .clone()
        .or_else(|| infer::get(&bytes).map(|kind| kind.mime_type().to_owned()))
        .unwrap_or_else(|| media_type_from_name(&selected.original_name).to_owned());
    preview_from_bytes(bytes, media_type)
}

#[tauri::command]
pub(crate) async fn thumbnail_attachment(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    attachment_id: String,
) -> Result<String, NativeResourceError> {
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
            "{}/sessions/{}/attachments/{}/thumbnail",
            bootstrap.base_url,
            session_id.as_str(),
            attachment_id.as_str()
        ))
        .bearer_auth(&bootstrap.access_token)
        .send()
        .await
        .map_err(|_| runtime_unavailable())?;
    if !response.status().is_success() {
        return Err(
            decode_runtime_failure(response, NativeResourceErrorCode::PreviewUnavailable).await,
        );
    }
    let bytes = response.bytes().await.map_err(|_| runtime_unavailable())?;
    if bytes.len() > MAX_PREVIEW_RESPONSE_BYTES {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "缩略图响应过大。",
        ));
    }
    Ok(format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes)))
}

#[tauri::command]
pub(crate) async fn preview_tool_file(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    child_task_id: Option<String>,
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
    let url = tool_resource_url(
        &bootstrap.base_url,
        &session_id,
        child_task_id.as_deref(),
        &message_id,
        &resource_ref_id,
        "preview",
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
    preview_from_bytes(bytes.to_vec(), media_type)
}

fn preview_from_bytes(
    bytes: Vec<u8>,
    media_type: String,
) -> Result<AttachmentPreview, NativeResourceError> {
    let size_bytes = bytes.len() as u64;
    if media_type.starts_with("image/") {
        let data_url = format!("data:{media_type};base64,{}", STANDARD.encode(&bytes));
        return Ok(AttachmentPreview {
            kind: PreviewKind::Image,
            media_type,
            size_bytes,
            text: None,
            data_url: Some(data_url),
            data_base64: None,
        });
    }
    if media_type.starts_with("application/pdf") && is_pdf(&bytes) {
        return Ok(AttachmentPreview {
            kind: PreviewKind::Pdf,
            media_type,
            size_bytes,
            text: None,
            data_url: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if media_type.starts_with("text/") || media_type.starts_with("application/json") {
        let text = String::from_utf8(bytes).map_err(|_| {
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
            data_base64: None,
        });
    }
    Err(error(
        NativeResourceErrorCode::ResourceNotPreviewable,
        "该文件类型不支持应用内预览。",
    ))
}

async fn validate_local_file(path: PathBuf) -> Result<PathBuf, NativeResourceError> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| unavailable("本地文件不存在或不可读取。"))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| unavailable("本地文件不存在或不可读取。"))?;
    if !metadata.is_file() {
        return Err(error(
            NativeResourceErrorCode::ResourceNotPreviewable,
            "只能打开普通文件。",
        ));
    }
    Ok(canonical)
}

async fn validate_registered_resource(
    resource: &LocalResourceHandle,
) -> Result<PathBuf, NativeResourceError> {
    let canonical = validate_local_file(resource.path.clone()).await?;
    if !canonical.starts_with(&resource.navigation_root) {
        return Err(error(
            NativeResourceErrorCode::ResourceOutsideRoot,
            "本地资源位于当前文件范围之外。",
        ));
    }
    Ok(canonical)
}

async fn preview_local_path(path: &Path) -> Result<LocalResourcePreview, NativeResourceError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| unavailable("本地文件不存在或不可读取。"))?;
    let file_name = display_file_name(path)?;
    let extension_media_type = media_type_from_name(&file_name).to_owned();
    if metadata.len() > MAX_IMAGE_PREVIEW_BYTES {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "文件超过 16 MiB 预览上限。",
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| unavailable("无法读取本地文件。"))?;
    if is_pdf(&bytes) {
        return Ok(LocalResourcePreview {
            kind: PreviewKind::Pdf,
            media_type: "application/pdf".to_owned(),
            size_bytes: bytes.len() as u64,
            text: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if let Some(kind) = infer::get(&bytes).filter(|kind| kind.mime_type().starts_with("image/")) {
        if bytes.len() as u64 > MAX_IMAGE_PREVIEW_BYTES {
            return Err(error(
                NativeResourceErrorCode::ResourceTooLarge,
                "图片超过 16 MiB 预览限制。",
            ));
        }
        let decoded = image::load_from_memory(&bytes).map_err(|_| {
            error(
                NativeResourceErrorCode::ResourceNotPreviewable,
                "图片内容无效或格式不受支持。",
            )
        })?;
        let (width, height) = decoded.dimensions();
        if width > MAX_IMAGE_EDGE
            || height > MAX_IMAGE_EDGE
            || u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS
        {
            return Err(error(
                NativeResourceErrorCode::ResourceTooLarge,
                "图片尺寸超过预览限制。",
            ));
        }
        return Ok(LocalResourcePreview {
            kind: PreviewKind::Image,
            media_type: kind.mime_type().to_owned(),
            size_bytes: bytes.len() as u64,
            text: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if bytes.len() as u64 > MAX_TEXT_PREVIEW_BYTES {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "文本文件超过 4 MiB 预览限制。",
        ));
    }
    if bytes.contains(&0) {
        return Err(error(
            NativeResourceErrorCode::ResourceNotPreviewable,
            "该文件不是可预览的文本、图片或 PDF。",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        error(
            NativeResourceErrorCode::ResourceNotPreviewable,
            "文件不是有效的 UTF-8 文本。",
        )
    })?;
    Ok(LocalResourcePreview {
        kind: PreviewKind::Text,
        media_type: extension_media_type,
        size_bytes: text.len() as u64,
        text: Some(text),
        data_base64: None,
    })
}

fn file_uri_path(file_uri: &str) -> Result<PathBuf, NativeResourceError> {
    let parsed = url::Url::parse(file_uri).map_err(|_| invalid_local_resource())?;
    if parsed.scheme() != "file"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_some_and(|host| host != "localhost")
    {
        return Err(invalid_local_resource());
    }
    parsed.to_file_path().map_err(|_| invalid_local_resource())
}

fn relative_reference_path(reference: &str) -> Result<PathBuf, NativeResourceError> {
    let without_fragment = reference.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode(without_fragment)?;
    if decoded.contains(':') {
        return Err(invalid_local_resource());
    }
    let path = Path::new(&decoded);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            )
        })
    {
        return Err(invalid_local_resource());
    }
    Ok(path.to_path_buf())
}

fn percent_decode(value: &str) -> Result<String, NativeResourceError> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(invalid_local_resource());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    let query = format!("value={}", value.replace('+', "%2B"));
    let decoded = url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "value")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(invalid_local_resource)?;
    if decoded.contains('\0') {
        return Err(invalid_local_resource());
    }
    Ok(decoded)
}

fn display_file_name(path: &Path) -> Result<String, NativeResourceError> {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| unavailable("资源名称不是有效文本。"))
}

fn display_path_segments(path: &Path) -> Result<Vec<String>, NativeResourceError> {
    let mut segments = Vec::new();
    for component in path.components() {
        let text = match component {
            std::path::Component::RootDir => "/".to_owned(),
            std::path::Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| unavailable("资源路径不是有效文本。"))?,
            _ => continue,
        };
        segments.push(text);
    }
    Ok(segments)
}

fn sibling_order(kind: LocalResourceSiblingKind) -> u8 {
    match kind {
        LocalResourceSiblingKind::Directory => 0,
        LocalResourceSiblingKind::File => 1,
    }
}

fn invalid_local_resource() -> NativeResourceError {
    error(
        NativeResourceErrorCode::InvalidRequest,
        "本地文件链接无效。",
    )
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
pub(crate) async fn copy_attachment_path(
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
    copy_path_to_clipboard(&attachment.agent_readable_path)
}

#[tauri::command]
pub(crate) async fn open_tool_file_in_system(
    app: AppHandle,
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    child_task_id: Option<String>,
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
        child_task_id.as_deref(),
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
    child_task_id: Option<String>,
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
        child_task_id.as_deref(),
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

#[tauri::command]
pub(crate) async fn copy_tool_file_path(
    bridge: State<'_, NativeResourceBridge>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    session_id: String,
    child_task_id: Option<String>,
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
        child_task_id.as_deref(),
        &message_id,
        &resource_ref_id,
    )
    .await?;
    copy_path_to_clipboard(&resource.path)
}

async fn get_tool_file_native_path(
    bridge: &NativeResourceBridge,
    coordinator: &RuntimeBootstrapCoordinator,
    session_id: &SessionId,
    child_task_id: Option<&str>,
    message_id: &MessageId,
    resource_ref_id: &ResourceRefId,
) -> Result<NativeResourcePath, NativeResourceError> {
    let bootstrap = coordinator
        .bootstrap()
        .await
        .map_err(|_| runtime_unavailable())?;
    let url = tool_resource_url(
        &bootstrap.base_url,
        session_id,
        child_task_id,
        message_id,
        resource_ref_id,
        "native-path",
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
    let part = selected_part(selected).await?;
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

async fn materialize_selected(
    http: &reqwest::Client,
    base_url: &str,
    access_token: &str,
    manifest: &SessionMaterializationManifest,
    selected: &[(String, SelectedAttachment)],
    cancellation: CancellationToken,
) -> Result<SessionMaterializationResult, NativeResourceError> {
    let manifest_json = serde_json::to_vec(manifest).map_err(|_| {
        error(
            NativeResourceErrorCode::InvalidRequest,
            "无法编码首次发送内容。",
        )
    })?;
    let mut form = Form::new().part("manifest", Part::bytes(manifest_json));
    for (selection_id, attachment) in selected {
        let part = selected_part(attachment).await?;
        form = form.part(selection_id.clone(), part);
    }
    let request = http
        .post(format!("{base_url}/session-materializations"))
        .bearer_auth(access_token)
        .multipart(form)
        .send();
    let response = tokio::select! {
        () = cancellation.cancelled() => {
            return Err(error(NativeResourceErrorCode::Cancelled, "首次发送已取消。"));
        }
        response = request => response.map_err(|_| error(
            NativeResourceErrorCode::MaterializationResponseUnknown,
            "未能确认 Runtime 是否已接受首次发送，请直接重试。",
        ))?,
    };
    if !response.status().is_success() {
        return Err(decode_runtime_failure(
            response,
            NativeResourceErrorCode::MaterializationFailed,
        )
        .await);
    }
    response
        .json::<SessionMaterializationResult>()
        .await
        .map_err(|_| {
            error(
                NativeResourceErrorCode::MaterializationResponseUnknown,
                "Runtime 已响应，但结果无法确认，请直接重试。",
            )
        })
}

async fn selected_part(selected: &SelectedAttachment) -> Result<Part, NativeResourceError> {
    let body = match &selected.source {
        SelectedAttachmentSource::ExternalPath(path) => {
            let file = tokio::fs::File::open(path).await.map_err(|_| {
                error(
                    NativeResourceErrorCode::SelectionUnavailable,
                    "无法读取所选附件。",
                )
            })?;
            reqwest::Body::wrap_stream(ReaderStream::new(file))
        }
        SelectedAttachmentSource::OwnedTempFile(_) => {
            reqwest::Body::from(read_selected_bytes(selected, None).await?)
        }
    };
    Ok(Part::stream_with_length(body, selected.size_bytes)
        .file_name(selected.original_name.clone()))
}

async fn read_selected_bytes(
    selected: &SelectedAttachment,
    maximum: Option<usize>,
) -> Result<Vec<u8>, NativeResourceError> {
    let bytes = match &selected.source {
        SelectedAttachmentSource::ExternalPath(path) => {
            let mut file = tokio::fs::File::open(path).await.map_err(|_| {
                error(
                    NativeResourceErrorCode::SelectionUnavailable,
                    "无法读取所选附件。",
                )
            })?;
            let mut bytes = Vec::new();
            if let Some(maximum) = maximum {
                file.take(maximum.saturating_add(1) as u64)
                    .read_to_end(&mut bytes)
                    .await
            } else {
                file.read_to_end(&mut bytes).await
            }
            .map_err(|_| unavailable("无法读取所选附件。"))?;
            bytes
        }
        SelectedAttachmentSource::OwnedTempFile(file) => {
            let file = Arc::clone(file);
            let size = usize::try_from(selected.size_bytes).map_err(|_| {
                error(
                    NativeResourceErrorCode::ResourceTooLarge,
                    "附件大小超出当前平台限制。",
                )
            })?;
            tokio::task::spawn_blocking(move || read_owned_file(&file, size))
                .await
                .map_err(|_| unavailable("无法读取剪贴板图片。"))?
                .map_err(|_| unavailable("无法读取剪贴板图片。"))?
        }
    };
    if maximum.is_some_and(|maximum| bytes.len() > maximum) {
        return Err(error(
            NativeResourceErrorCode::ResourceTooLarge,
            "附件预览超过大小限制。",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_owned_file(file: &File, size: usize) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;

    let mut bytes = vec![0; size];
    let mut offset = 0;
    while offset < size {
        let read = file.read_at(&mut bytes[offset..], offset as u64)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "clipboard temp file ended early",
            ));
        }
        offset += read;
    }
    Ok(bytes)
}

#[cfg(windows)]
fn read_owned_file(file: &File, size: usize) -> std::io::Result<Vec<u8>> {
    use std::os::windows::fs::FileExt as _;

    let mut bytes = vec![0; size];
    let mut offset = 0;
    while offset < size {
        let read = file.seek_read(&mut bytes[offset..], offset as u64)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "clipboard temp file ended early",
            ));
        }
        offset += read;
    }
    Ok(bytes)
}

fn decode_header_value(value: &str) -> Option<String> {
    let query = format!("value={value}");
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "value")
        .map(|(_, value)| value.into_owned())
}

fn safe_original_name(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Path::new(value)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(str::to_owned)
}

fn media_type_from_name(name: &str) -> &'static str {
    match Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some(
            "txt" | "md" | "csv" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "css" | "scss"
            | "html" | "xml" | "toml" | "yaml" | "yml",
        ) => "text/plain",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn is_pdf(bytes: &[u8]) -> bool {
    bytes
        .get(..bytes.len().min(1_024))
        .is_some_and(|header| header.windows(5).any(|window| window == b"%PDF-"))
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
            assistant_protocol::RuntimeErrorCode::InvalidRequest => {
                NativeResourceErrorCode::InvalidRequest
            }
            assistant_protocol::RuntimeErrorCode::OperationNotAllowed => {
                NativeResourceErrorCode::ResourceOutsideRoot
            }
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

fn tool_resource_url(
    base_url: &str,
    session_id: &SessionId,
    child_task_id: Option<&str>,
    message_id: &MessageId,
    resource_ref_id: &ResourceRefId,
    operation: &str,
) -> Result<reqwest::Url, NativeResourceError> {
    if let Some(child_task_id) = child_task_id {
        let child_task_id = ChildTaskId::new(child_task_id.to_owned())
            .map_err(|_| error(NativeResourceErrorCode::InvalidRequest, "子任务标识无效。"))?;
        return runtime_resource_url(
            base_url,
            &[
                "sessions",
                session_id.as_str(),
                "child-tasks",
                child_task_id.as_str(),
                "messages",
                message_id.as_str(),
                "resources",
                resource_ref_id.as_str(),
                operation,
            ],
        );
    }
    runtime_resource_url(
        base_url,
        &[
            "sessions",
            session_id.as_str(),
            "messages",
            message_id.as_str(),
            "resources",
            resource_ref_id.as_str(),
            operation,
        ],
    )
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
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

    #[tokio::test]
    async fn clipboard_image_uses_magic_bytes_and_owned_selection_reader() {
        let bridge = NativeResourceBridge::new();
        let selected =
            stage_clipboard_image_bytes(&bridge, PNG, "image/jpeg", Some("folder/screenshot.png"))
                .expect("stage image");

        assert_eq!(selected.original_name, "screenshot.png");
        assert_eq!(selected.media_type.as_deref(), Some("image/png"));
        assert!(matches!(
            selected.origin,
            AttachmentSelectionOrigin::Clipboard
        ));
        let attachment = bridge.selected(&selected.selection_id).expect("selection");
        assert_eq!(
            read_selected_bytes(&attachment, Some(MAX_PREVIEW_RESPONSE_BYTES))
                .await
                .expect("owned bytes"),
            PNG
        );
    }

    #[test]
    fn clipboard_image_rejects_fake_mime_and_process_temp_limit() {
        let bridge = NativeResourceBridge::new();
        let fake =
            stage_clipboard_image_bytes(&bridge, b"not an image", "image/png", Some("fake.png"))
                .expect_err("fake image must fail");
        assert!(matches!(fake.code, NativeResourceErrorCode::InvalidRequest));

        let owned = tempfile::tempfile().expect("temp file");
        bridge.selections.lock().expect("selections").insert(
            "existing".to_owned(),
            SelectedAttachment {
                source: SelectedAttachmentSource::OwnedTempFile(Arc::new(owned)),
                original_name: "existing.png".to_owned(),
                size_bytes: MAX_CLIPBOARD_TEMP_BYTES,
                media_type: Some("image/png".to_owned()),
                selected_at_ms: now_ms(),
            },
        );
        let limited = stage_clipboard_image_bytes(&bridge, PNG, "image/png", Some("next.png"))
            .expect_err("process total must fail");
        assert!(matches!(
            limited.code,
            NativeResourceErrorCode::SelectionLimitReached
        ));
        assert_eq!(bridge.selections.lock().expect("selections").len(), 1);
    }

    #[test]
    fn export_name_never_contains_path_components() {
        assert_eq!(export_file_name("a/b:c"), "a_b_c.md");
        assert_eq!(export_file_name("  "), "会话.md");
    }

    #[test]
    fn tool_resource_url_keeps_main_and_child_owner_namespaces_distinct() {
        let session_id = SessionId::new("session-1").expect("session id");
        let message_id = MessageId::new("message-1").expect("message id");
        let resource_ref_id = ResourceRefId::new("resource-1").expect("resource id");
        let main = tool_resource_url(
            "http://127.0.0.1:1234",
            &session_id,
            None,
            &message_id,
            &resource_ref_id,
            "preview",
        )
        .expect("main url");
        let child = tool_resource_url(
            "http://127.0.0.1:1234",
            &session_id,
            Some("child-1"),
            &message_id,
            &resource_ref_id,
            "preview",
        )
        .expect("child url");

        assert_eq!(
            main.path(),
            "/sessions/session-1/messages/message-1/resources/resource-1/preview"
        );
        assert_eq!(
            child.path(),
            "/sessions/session-1/child-tasks/child-1/messages/message-1/resources/resource-1/preview"
        );
    }

    #[test]
    fn temporary_export_stays_in_selected_directory() {
        let parent = Path::new("/tmp/export-target");
        let path = temporary_export_path(parent, "export-1");
        assert_eq!(path.parent(), Some(parent));
    }

    #[test]
    fn local_file_uri_accepts_encoded_local_paths_and_rejects_remote_or_qualified_urls() {
        assert_eq!(
            file_uri_path("file:///tmp/%E6%8A%A5%E5%91%8A%20final.md").expect("encoded local path"),
            PathBuf::from("/tmp/报告 final.md")
        );
        assert_eq!(
            file_uri_path("file://localhost/tmp/report.md").expect("localhost path"),
            PathBuf::from("/tmp/report.md")
        );
        for invalid in [
            "https://example.com/report.md",
            "file://example.com/tmp/report.md",
            "file:///tmp/report.md?download=1",
            "file:///tmp/report.md#section",
        ] {
            assert!(file_uri_path(invalid).is_err(), "must reject {invalid}");
        }
    }

    #[test]
    fn relative_resource_reference_decodes_unicode_without_accepting_absolute_paths() {
        assert_eq!(
            relative_reference_path("images/%E6%88%AA%E5%9B%BE%20one.png").expect("relative path"),
            PathBuf::from("images/截图 one.png")
        );
        assert!(relative_reference_path("/tmp/report.md").is_err());
        assert!(relative_reference_path("%2Ftmp/report.md").is_err());
        assert!(relative_reference_path("file:///tmp/report.md").is_err());
        assert!(relative_reference_path("bad%2/path.md").is_err());
    }

    #[tokio::test]
    async fn local_preview_uses_content_boundaries_for_text_and_binary_files() {
        let directory = TempDir::new().expect("resource directory");
        let text_path = directory.path().join("报告 final.md");
        fs::write(&text_path, "# 结果\n").expect("text fixture");
        let text = preview_local_path(&text_path).await.expect("text preview");
        assert!(matches!(text.kind, PreviewKind::Text));
        assert_eq!(text.text.as_deref(), Some("# 结果\n"));

        let binary_path = directory.path().join("fake.txt");
        fs::write(&binary_path, b"prefix\0suffix").expect("binary fixture");
        let binary = preview_local_path(&binary_path)
            .await
            .expect_err("binary content must not become text");
        assert!(matches!(
            binary.code,
            NativeResourceErrorCode::ResourceNotPreviewable
        ));

        let pdf_path = directory.path().join("document.data");
        let pdf_bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";
        fs::write(&pdf_path, pdf_bytes).expect("PDF fixture");
        let pdf = preview_local_path(&pdf_path).await.expect("PDF preview");
        assert!(matches!(pdf.kind, PreviewKind::Pdf));
        assert_eq!(pdf.media_type, "application/pdf");
        assert_eq!(
            pdf.data_base64.as_deref(),
            Some(STANDARD.encode(pdf_bytes).as_str())
        );

        let oversized_path = directory.path().join("large.txt");
        fs::write(
            &oversized_path,
            vec![b'a'; MAX_TEXT_PREVIEW_BYTES as usize + 1],
        )
        .expect("oversized fixture");
        let oversized = preview_local_path(&oversized_path)
            .await
            .expect_err("oversized text must fail");
        assert!(matches!(
            oversized.code,
            NativeResourceErrorCode::ResourceTooLarge
        ));
    }
}
