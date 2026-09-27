//! 插件原包的独立流式入口；包字节永不经过 JSON Command 或附件目录。

use std::{path::PathBuf, sync::Arc, time::Duration};

use assistant_protocol::{
    IdempotencyKey, PluginArchiveFormat, PluginPackageOrigin, PluginPackageSelection,
    RuntimeErrorCode, RuntimeErrorInfo,
};
use assistant_runtime::AssistantRuntime;
use axum::{
    Extension, Json,
    body::Body,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::{ReadyServices, error::runtime_status};
use crate::{
    access::AccessPermit,
    plugins::{ArchiveFormat, HostPluginFiles, prepare_archive_with_cancel},
};

const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

struct ProbeCancellation(tokio_util::sync::CancellationToken);

impl Drop for ProbeCancellation {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) async fn inspect_plugin_package(
    Extension(services): Extension<Arc<ReadyServices>>,
    Extension(permit): Extension<AccessPermit>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    let (format, archive_format) = match headers
        .get("x-ez-plugin-format")
        .and_then(|value| value.to_str().ok())
    {
        Some("zip") => (ArchiveFormat::Zip, PluginArchiveFormat::Zip),
        Some("tar_gzip") => (ArchiveFormat::TarGzip, PluginArchiveFormat::TarGzip),
        _ => return invalid("plugin archive format is invalid"),
    };
    let inspection = match tempfile::Builder::new()
        .prefix("plugin-inspect-")
        .tempdir_in(&services.paths.user_root)
    {
        Ok(inspection) => inspection,
        Err(_) => return unavailable(),
    };
    let archive = inspection.path().join("archive");
    let cancellation = ProbeCancellation(services.cancellation.child_token());
    let result = async {
        let mut file = tokio::fs::File::create(&archive)
            .await
            .map_err(|_| unavailable())?;
        let mut stream = body.into_data_stream();
        let started = tokio::time::Instant::now();
        let mut size = 0_u64;
        loop {
            if started.elapsed() >= TOTAL_TIMEOUT || cancellation.0.is_cancelled() {
                return Err(invalid("plugin inspection was interrupted"));
            }
            let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
                Ok(Some(Ok(chunk))) => chunk,
                Ok(None) => break,
                _ => return Err(invalid("plugin inspection stream was interrupted")),
            };
            size = size
                .checked_add(chunk.len() as u64)
                .filter(|value| *value <= MAX_PACKAGE_BYTES)
                .ok_or_else(|| invalid("plugin package exceeds size limit"))?;
            file.write_all(&chunk).await.map_err(|_| unavailable())?;
        }
        file.flush().await.map_err(|_| unavailable())?;
        file.sync_all().await.map_err(|_| unavailable())?;
        drop(file);
        if size == 0 || permit.check().is_err() {
            return Err(invalid("plugin inspection is not authorized"));
        }
        let archive_path = archive.clone();
        let staging = inspection.path().to_path_buf();
        let token = cancellation.0.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_archive_with_cancel(&archive_path, format, &staging, &token)
        })
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| invalid("plugin package is invalid or unsupported"))?;
        let selection = PluginPackageSelection {
            name: prepared.manifest.name.clone(),
            version: prepared.manifest.version.clone(),
            sha256: prepared.sha256.clone(),
            size_bytes: size,
            format: archive_format,
            origin: PluginPackageOrigin::Upload,
            expected_active_sha256: None,
        };
        prepared.staging.close().map_err(|_| unavailable())?;
        Ok(selection)
    }
    .await;
    drop(cancellation);
    if inspection.close().is_err() {
        return unavailable();
    }
    match result {
        Ok(selection) => (StatusCode::OK, Json(selection)).into_response(),
        Err(response) => response,
    }
}

#[derive(Serialize)]
struct UploadError {
    error: RuntimeErrorInfo,
}

/// HTTP future 意外被取消时也移除尚未完成的文件，持久操作留待 Runtime 恢复。
struct IncompleteUpload {
    path: PathBuf,
    key: IdempotencyKey,
    runtime: Arc<AssistantRuntime>,
    completed: bool,
}

impl Drop for IncompleteUpload {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let _ = std::fs::remove_file(&self.path);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let runtime = self.runtime.clone();
            let key = self.key.clone();
            handle.spawn(async move {
                let _ = runtime.plugin_upload_failed(&key, true).await;
            });
        }
    }
}

pub(super) async fn upload_plugin_package(
    Extension(services): Extension<Arc<ReadyServices>>,
    Extension(permit): Extension<AccessPermit>,
    Path(key): Path<String>,
    body: Body,
) -> Response {
    let key = match IdempotencyKey::new(key) {
        Ok(key) => key,
        Err(_) => return invalid("plugin operation key is invalid"),
    };
    let selection = match services.runtime.plugin_upload_selection(&key).await {
        Ok(selection) => selection,
        Err(error) => return failure(error.to_protocol_info()),
    };
    let files = HostPluginFiles::new(services.paths.user_root.clone());
    let path = match files.upload_path(&key) {
        Ok(path) => path,
        Err(_) => {
            let _ = services.runtime.plugin_upload_failed(&key, false).await;
            return unavailable();
        }
    };
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(_) => {
            let _ = services.runtime.plugin_upload_failed(&key, false).await;
            return unavailable();
        }
    };
    let mut guard = IncompleteUpload {
        path,
        key: key.clone(),
        runtime: services.runtime.clone(),
        completed: false,
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let started = tokio::time::Instant::now();
    let mut size = 0_u64;
    let mut hash = Sha256::new();
    loop {
        if started.elapsed() >= TOTAL_TIMEOUT || services.cancellation.is_cancelled() {
            return invalid("plugin upload was interrupted");
        }
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => break,
            _ => return invalid("plugin upload stream was interrupted"),
        };
        size = match size.checked_add(chunk.len() as u64) {
            Some(size) if size <= selection.size_bytes => size,
            _ => return invalid("plugin upload exceeds confirmed size"),
        };
        services
            .runtime
            .report_plugin_upload_progress(&key, size, selection.size_bytes);
        hash.update(&chunk);
        if file.write_all(&chunk).await.is_err() {
            return unavailable();
        }
        if size % (1024 * 1024) < chunk.len() as u64
            && let Ok(operation) = services
                .runtime
                .get_plugin_operation(assistant_protocol::PluginOperationRequest {
                    key: key.clone(),
                })
                .await
            && operation.cancel_requested
        {
            return invalid("plugin upload was cancelled");
        }
    }
    if size != selection.size_bytes || format!("{:x}", hash.finalize()) != selection.sha256 {
        return invalid("plugin package does not match confirmed size or SHA-256");
    }
    if file.flush().await.is_err() || file.sync_all().await.is_err() || permit.check().is_err() {
        return unavailable();
    }
    drop(file);
    guard.completed = true;
    match services.runtime.plugin_upload_received(&key).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(error) => {
            let _ = services.runtime.plugin_upload_failed(&key, false).await;
            failure(error.to_protocol_info())
        }
    }
}

fn invalid(message: &'static str) -> Response {
    failure(RuntimeErrorInfo::new(
        RuntimeErrorCode::InvalidRequest,
        message,
    ))
}
fn unavailable() -> Response {
    failure(RuntimeErrorInfo::new(
        RuntimeErrorCode::StorageUnavailable,
        "plugin upload storage is unavailable",
    ))
}
fn failure(error: RuntimeErrorInfo) -> Response {
    (runtime_status(error.code), Json(UploadError { error })).into_response()
}
