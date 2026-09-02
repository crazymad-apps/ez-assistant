//! 新会话首次发送的批量流式 multipart 入口。

use std::path::PathBuf;

use assistant_protocol::{RuntimeErrorCode, RuntimeErrorInfo, SessionMaterializationManifest};
use assistant_runtime::StagedSessionAttachment;
use axum::{
    Json,
    extract::{Multipart, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::Digest;
use tokio::io::AsyncWriteExt;

use super::{
    HttpState, MAX_ATTACHMENT_BYTES, MAX_COMMAND_BYTES,
    attachments::{cleanup, create_staging_file, runtime_error, valid_original_name},
    error::runtime_status,
};
use crate::attachment_hash;

const MAX_MATERIALIZATION_ATTACHMENTS: usize = 32;

#[derive(Serialize)]
struct MaterializationErrorBody {
    error: RuntimeErrorInfo,
}

pub(super) async fn materialize_session(
    State(state): State<HttpState>,
    mut multipart: Multipart,
) -> Response {
    let manifest = match read_manifest(&mut multipart).await {
        Ok(value) => value,
        Err(error) => return materialization_error(error),
    };
    if manifest.attachments.len() > MAX_MATERIALIZATION_ATTACHMENTS {
        return materialization_error(invalid_materialization(
            "materialization contains too many files",
        ));
    }

    let mut staged = Vec::with_capacity(manifest.attachments.len());
    let mut total_bytes = 0_u64;
    for declared in &manifest.attachments {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => {
                cleanup_all(&staged).await;
                return materialization_error(invalid_materialization(
                    "materialization file field is missing",
                ));
            }
            Err(_) => {
                cleanup_all(&staged).await;
                return materialization_error(invalid_materialization(
                    "materialization file field is invalid",
                ));
            }
        };
        if field.name() != Some(declared.selection_key.as_str())
            || field.file_name() != Some(declared.original_name.as_str())
            || !valid_original_name(&declared.original_name)
        {
            cleanup_all(&staged).await;
            return materialization_error(invalid_materialization(
                "materialization file does not match manifest",
            ));
        }
        let (staging_path, mut staging_file) = match create_staging_file(&state).await {
            Ok(value) => value,
            Err(_) => {
                cleanup_all(&staged).await;
                return materialization_error(storage_unavailable());
            }
        };
        let mut hasher = attachment_hash::new_hasher(&declared.original_name);
        let mut size_bytes = 0_u64;
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    cleanup(&staging_path).await;
                    cleanup_all(&staged).await;
                    return materialization_error(invalid_materialization(
                        "materialization file stream is invalid",
                    ));
                }
            };
            size_bytes = match size_bytes.checked_add(chunk.len() as u64) {
                Some(size) if size <= MAX_ATTACHMENT_BYTES => size,
                _ => {
                    cleanup(&staging_path).await;
                    cleanup_all(&staged).await;
                    return materialization_error(too_large());
                }
            };
            total_bytes = match total_bytes.checked_add(chunk.len() as u64) {
                Some(size) if size <= MAX_ATTACHMENT_BYTES => size,
                _ => {
                    cleanup(&staging_path).await;
                    cleanup_all(&staged).await;
                    return materialization_error(too_large());
                }
            };
            hasher.update(&chunk);
            if staging_file.write_all(&chunk).await.is_err() {
                cleanup(&staging_path).await;
                cleanup_all(&staged).await;
                return materialization_error(storage_unavailable());
            }
        }
        drop(field);
        if size_bytes != declared.size_bytes {
            cleanup(&staging_path).await;
            cleanup_all(&staged).await;
            return materialization_error(invalid_materialization(
                "materialization file size does not match manifest",
            ));
        }
        if staging_file.flush().await.is_err() || staging_file.sync_all().await.is_err() {
            cleanup(&staging_path).await;
            cleanup_all(&staged).await;
            return materialization_error(storage_unavailable());
        }
        drop(staging_file);
        let media_type = match tokio::task::spawn_blocking({
            let path = staging_path.clone();
            move || crate::image::sniff_media_type(&path)
        })
        .await
        {
            Ok(Ok(value)) => Some(value),
            _ => {
                cleanup(&staging_path).await;
                cleanup_all(&staged).await;
                return materialization_error(invalid_materialization(
                    "materialization file type could not be detected",
                ));
            }
        };
        staged.push(StagedSessionAttachment {
            selection_key: declared.selection_key.clone(),
            original_name: declared.original_name.clone(),
            staging_path: staging_path.to_string_lossy().into_owned(),
            blob_hash: format!("{:x}", hasher.finalize()),
            size_bytes,
            media_type,
        });
    }
    match multipart.next_field().await {
        Ok(None) => {}
        Ok(Some(_)) | Err(_) => {
            cleanup_all(&staged).await;
            return materialization_error(invalid_materialization(
                "materialization contains unexpected trailing fields",
            ));
        }
    }

    match state
        .runtime
        .materialize_session(manifest, staged.clone())
        .await
    {
        Ok(mut result) => {
            for attachment in &mut result.attachments {
                let source = PathBuf::from(&attachment.agent_readable_path);
                if attachment
                    .media_type
                    .as_deref()
                    .is_some_and(|value| value.starts_with("image/"))
                {
                    tokio::task::spawn_blocking(move || {
                        let _ = crate::image::ensure_thumbnail(&source);
                    });
                }
                attachment.agent_readable_path.clear();
            }
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(error) => {
            cleanup_all(&staged).await;
            runtime_error(error)
        }
    }
}

async fn read_manifest(
    multipart: &mut Multipart,
) -> Result<SessionMaterializationManifest, RuntimeErrorInfo> {
    let mut field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("manifest") && field.file_name().is_none() => field,
        Ok(_) => {
            return Err(invalid_materialization(
                "multipart first field must be the materialization manifest",
            ));
        }
        Err(_) => {
            return Err(invalid_materialization(
                "materialization manifest is invalid",
            ));
        }
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| invalid_materialization("materialization manifest is invalid"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_COMMAND_BYTES {
            return Err(RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceTooLarge,
                "materialization manifest exceeds the configured size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| invalid_materialization("materialization manifest JSON is invalid"))
}

async fn cleanup_all(staged: &[StagedSessionAttachment]) {
    for attachment in staged {
        cleanup(&PathBuf::from(&attachment.staging_path)).await;
    }
}

fn materialization_error(error: RuntimeErrorInfo) -> Response {
    (
        runtime_status(error.code),
        Json(MaterializationErrorBody { error }),
    )
        .into_response()
}

fn invalid_materialization(message: &'static str) -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(RuntimeErrorCode::AttachmentUploadInvalid, message)
}

fn storage_unavailable() -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(
        RuntimeErrorCode::StorageUnavailable,
        "runtime storage is unavailable",
    )
}

fn too_large() -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(
        RuntimeErrorCode::AttachmentTooLarge,
        "materialization attachments exceed the configured size limit",
    )
}
