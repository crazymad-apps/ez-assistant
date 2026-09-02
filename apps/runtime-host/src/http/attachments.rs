//! Session Attachment 的独立流式 multipart 上传入口。

use std::{io, path::PathBuf};

use assistant_protocol::{RuntimeErrorCode, RuntimeErrorInfo, SessionId};
use assistant_runtime::{RuntimeError, StagedAttachmentUpload};
use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::Digest;
use tokio::io::AsyncWriteExt;

use super::{HttpState, MAX_ATTACHMENT_BYTES, error::runtime_status};
use crate::attachment_hash;

const RANDOM_NAME_BYTES: usize = 16;

#[derive(Serialize)]
struct UploadErrorBody {
    error: RuntimeErrorInfo,
}

pub(super) async fn upload_attachment(
    State(state): State<HttpState>,
    Path(session_id): Path<String>,
    mut multipart: Multipart,
) -> Response {
    let session_id = match SessionId::new(session_id) {
        Ok(value) => value,
        Err(_) => return upload_error(invalid_upload("session id is invalid")),
    };
    if let Err(error) = state.runtime.begin_attachment_upload(&session_id).await {
        return runtime_error(error);
    }

    let mut field = match multipart.next_field().await {
        Ok(Some(field)) if field.name() == Some("file") => field,
        Ok(_) => {
            return upload_error(invalid_upload(
                "multipart must contain exactly one file field",
            ));
        }
        Err(_) => return upload_error(invalid_upload("multipart first field is invalid")),
    };
    let original_name = match field.file_name() {
        Some(name) if valid_original_name(name) => name.to_owned(),
        _ => return upload_error(invalid_upload("uploaded file name is invalid")),
    };
    let (staging_path, mut staging) = match create_staging_file(&state).await {
        Ok(value) => value,
        Err(_) => return upload_error(storage_unavailable()),
    };

    let mut hasher = attachment_hash::new_hasher(&original_name);
    let mut size_bytes = 0_u64;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                cleanup(&staging_path).await;
                return upload_error(invalid_upload("multipart file stream is invalid"));
            }
        };
        size_bytes = match size_bytes.checked_add(chunk.len() as u64) {
            Some(size) if size <= MAX_ATTACHMENT_BYTES => size,
            _ => {
                cleanup(&staging_path).await;
                return upload_error(RuntimeErrorInfo::new(
                    RuntimeErrorCode::AttachmentTooLarge,
                    "attachment exceeds the configured size limit",
                ));
            }
        };
        hasher.update(&chunk);
        if staging.write_all(&chunk).await.is_err() {
            cleanup(&staging_path).await;
            return upload_error(storage_unavailable());
        }
    }
    drop(field);
    match multipart.next_field().await {
        Ok(None) => {}
        Ok(Some(_)) => {
            cleanup(&staging_path).await;
            return upload_error(invalid_upload(
                "multipart must contain exactly one file field",
            ));
        }
        Err(_) => {
            cleanup(&staging_path).await;
            return upload_error(invalid_upload("multipart trailing body is invalid"));
        }
    }
    if staging.flush().await.is_err() || staging.sync_all().await.is_err() {
        cleanup(&staging_path).await;
        return upload_error(storage_unavailable());
    }
    drop(staging);

    let media_type = match tokio::task::spawn_blocking({
        let path = staging_path.clone();
        move || crate::image::sniff_media_type(&path)
    })
    .await
    {
        Ok(Ok(value)) => Some(value),
        _ => {
            cleanup(&staging_path).await;
            return upload_error(invalid_upload("uploaded file type could not be detected"));
        }
    };

    let result = state
        .runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id,
            original_name,
            staging_path: staging_path.to_string_lossy().into_owned(),
            blob_hash: format!("{:x}", hasher.finalize()),
            size_bytes,
            media_type,
        })
        .await;
    match result {
        Ok(mut result) => {
            let thumbnail_source = PathBuf::from(&result.attachment.agent_readable_path);
            if result
                .attachment
                .media_type
                .as_deref()
                .is_some_and(|value| value.starts_with("image/"))
            {
                tokio::task::spawn_blocking(move || {
                    let _ = crate::image::ensure_thumbnail(&thumbnail_source);
                });
            }
            // Agent-readable storage paths stay inside Runtime/Host. The Desktop only needs the
            // stable Attachment identity and display metadata after a successful upload.
            result.attachment.agent_readable_path.clear();
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(error) => {
            cleanup(&staging_path).await;
            runtime_error(error)
        }
    }
}

pub(super) async fn create_staging_file(
    state: &HttpState,
) -> io::Result<(PathBuf, tokio::fs::File)> {
    for _ in 0..16 {
        let mut random = [0_u8; RANDOM_NAME_BYTES];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let path = state
            .upload_staging_directory
            .join(format!("{}.part", URL_SAFE_NO_PAD.encode(random)));
        match tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "attachment staging name collision",
    ))
}

pub(super) fn valid_original_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 1024
        && !name.contains('/')
        && !name.contains('\\')
}

pub(super) async fn cleanup(path: &PathBuf) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

pub(super) fn runtime_error(error: RuntimeError) -> Response {
    upload_error(error.to_protocol_info())
}

fn upload_error(error: RuntimeErrorInfo) -> Response {
    (runtime_status(error.code), Json(UploadErrorBody { error })).into_response()
}

fn invalid_upload(message: &'static str) -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(RuntimeErrorCode::AttachmentUploadInvalid, message)
}

fn storage_unavailable() -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(
        RuntimeErrorCode::StorageUnavailable,
        "runtime storage is unavailable",
    )
}
