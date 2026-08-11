//! 私有文件初始化、路径约束和存储错误转换。

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use assistant_protocol::{AttachmentId, SessionId, WorkspaceId};
use assistant_runtime::{StoreError, StoreErrorKind};
use rusqlite::ErrorCode;

use super::{PRIVATE_FILE_MODE, StorageResult};

pub(super) fn prepare_private_file(path: &Path) -> StorageResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(invalid_data("runtime database path is not a regular file")),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(PRIVATE_FILE_MODE)
                .open(path)
                .map_err(|source| {
                    internal_error("runtime database file could not be created", source)
                })?;
            file.sync_all().map_err(|source| {
                internal_error("runtime database file could not be synchronized", source)
            })?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
        Err(source) => {
            return Err(internal_error(
                "runtime database metadata could not be read",
                source,
            ));
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).map_err(|source| {
        internal_error("runtime database permissions could not be set", source)
    })?;
    Ok(())
}

pub(super) fn create_new_private_file(path: &Path) -> StorageResult<File> {
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                StoreError::with_source(
                    StoreErrorKind::Conflict,
                    "conversation file already exists",
                    source,
                )
            } else {
                internal_error("conversation file could not be created", source)
            }
        })?;
    file.sync_all()
        .map_err(|source| internal_error("conversation file could not be synchronized", source))?;
    Ok(file)
}

pub(super) fn sync_directory(path: &Path) -> StorageResult<()> {
    let directory = File::open(path)
        .map_err(|source| internal_error("runtime data directory could not be opened", source))?;
    directory.sync_all().map_err(|source| {
        internal_error("runtime data directory could not be synchronized", source)
    })
}

pub(super) fn body_path(session_directory: &Path, generation: u64) -> PathBuf {
    session_directory.join(format!("conversation.{generation}.jsonl"))
}

pub(super) fn validate_session_component(session_id: &SessionId) -> StorageResult<()> {
    validate_identifier_component(
        session_id.as_str(),
        "session id cannot be used by local runtime storage",
    )
}

pub(super) fn validate_workspace_component(workspace_id: &WorkspaceId) -> StorageResult<()> {
    validate_identifier_component(
        workspace_id.as_str(),
        "workspace id cannot be used by local runtime storage",
    )
}

pub(super) fn validate_attachment_component(attachment_id: &AttachmentId) -> StorageResult<()> {
    validate_identifier_component(
        attachment_id.as_str(),
        "attachment id cannot be used by local runtime storage",
    )
}

fn validate_identifier_component(value: &str, message: &'static str) -> StorageResult<()> {
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(StoreError::new(StoreErrorKind::InvalidInput, message));
    }
    Ok(())
}

pub(super) fn positive_u64(value: i64, message: &'static str) -> StorageResult<u64> {
    if value <= 0 {
        return Err(invalid_data(message));
    }
    u64::try_from(value).map_err(|source| invalid_data_with_source(message, source))
}

pub(super) fn non_negative_u64(value: i64, message: &'static str) -> StorageResult<u64> {
    u64::try_from(value).map_err(|source| invalid_data_with_source(message, source))
}

pub(super) fn to_i64(value: u64, message: &'static str) -> StorageResult<i64> {
    i64::try_from(value)
        .map_err(|source| StoreError::with_source(StoreErrorKind::InvalidInput, message, source))
}

pub(super) fn internal_error(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> StoreError {
    StoreError::with_source(StoreErrorKind::Internal, message, source)
}

pub(super) fn invalid_data(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::InvalidData, message)
}

pub(super) fn invalid_data_with_source(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> StoreError {
    StoreError::with_source(StoreErrorKind::InvalidData, message, source)
}

pub(super) fn conflict(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::Conflict, message)
}

pub(super) fn database_write_error(message: &'static str, source: rusqlite::Error) -> StoreError {
    let kind = match &source {
        rusqlite::Error::SqliteFailure(error, _)
            if error.code == ErrorCode::ConstraintViolation =>
        {
            StoreErrorKind::Conflict
        }
        _ => StoreErrorKind::Internal,
    };
    StoreError::with_source(kind, message, source)
}
