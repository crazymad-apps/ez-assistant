//! Attachment 文件名、Blob 路径和 Session 稳定视图的局部文件系统规则。

use std::{
    fs, io,
    io::{BufReader, Read},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
};

use assistant_protocol::AttachmentId;
use assistant_runtime::{StoreError, StoreErrorKind};
use sha2::Digest;

use super::{StorageResult, internal_error, invalid_data};
use crate::attachment_hash;
use crate::config_source::prepare_private_directory;

pub(super) fn validate_original_name(name: &str) -> StorageResult<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 1024
        || name.contains('/')
        || name.contains('\\')
        || Path::new(name)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(StoreError::new(
            StoreErrorKind::InvalidInput,
            "attachment file name is invalid",
        ));
    }
    Ok(())
}

/// 只用于稳定视图的可读文件名；原始名称仍完整保存在数据库中。
pub(super) fn safe_display_name(name: &str) -> String {
    let mut result = String::new();
    for character in name.chars() {
        let character = if character.is_control() {
            '_'
        } else {
            character
        };
        if result.len() + character.len_utf8() > 180 {
            break;
        }
        result.push(character);
    }
    if result.is_empty() || result == "." || result == ".." {
        "attachment".to_owned()
    } else {
        result
    }
}

pub(super) fn blob_relative_path(blob_hash: &str) -> PathBuf {
    PathBuf::from("blobs")
        .join("sha256")
        .join(&blob_hash[..2])
        .join(blob_hash)
}

pub(super) fn validate_blob_hash(blob_hash: &str) -> StorageResult<()> {
    if blob_hash.len() != 64
        || !blob_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::new(
            StoreErrorKind::InvalidInput,
            "attachment blob hash is invalid",
        ));
    }
    Ok(())
}

pub(super) fn validate_staging_path(
    staging_directory: &Path,
    staging_path: &Path,
    expected_size: u64,
) -> StorageResult<()> {
    if staging_path.parent() != Some(staging_directory)
        || staging_path.extension().and_then(|value| value.to_str()) != Some("part")
    {
        return Err(StoreError::new(
            StoreErrorKind::InvalidInput,
            "attachment staging path is invalid",
        ));
    }
    let metadata = fs::symlink_metadata(staging_path).map_err(|source| {
        internal_error("attachment staging file could not be inspected", source)
    })?;
    if !metadata.file_type().is_file() || metadata.len() != expected_size {
        return Err(StoreError::new(
            StoreErrorKind::InvalidInput,
            "attachment staging file is invalid",
        ));
    }
    Ok(())
}

pub(super) fn ensure_blob(
    data_directory: &Path,
    staging_path: &Path,
    original_name: &str,
    blob_hash: &str,
    expected_size: u64,
) -> StorageResult<(PathBuf, bool)> {
    let relative = blob_relative_path(blob_hash);
    let blob = data_directory.join(&relative);
    let parent = blob.parent().expect("blob path has a parent");
    prepare_private_directory(parent).map_err(|source| {
        internal_error("attachment blob directory could not be prepared", source)
    })?;
    match fs::symlink_metadata(&blob) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.len() != expected_size {
                return Err(invalid_data("stored attachment blob is invalid"));
            }
            if hash_blob_file(&blob, original_name)? != blob_hash {
                return Err(invalid_data("stored attachment blob hash is invalid"));
            }
            fs::remove_file(staging_path).map_err(|source| {
                internal_error(
                    "duplicate attachment staging file could not be removed",
                    source,
                )
            })?;
            Ok((relative, false))
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::rename(staging_path, &blob).map_err(|source| {
                internal_error("attachment blob could not be committed", source)
            })?;
            fs::set_permissions(&blob, fs::Permissions::from_mode(0o400)).map_err(|source| {
                internal_error("attachment blob permissions could not be set", source)
            })?;
            super::sync_directory(parent)?;
            Ok((relative, true))
        }
        Err(source) => Err(internal_error(
            "attachment blob could not be inspected",
            source,
        )),
    }
}

fn hash_blob_file(path: &Path, original_name: &str) -> StorageResult<String> {
    let file = fs::File::open(path)
        .map_err(|source| internal_error("attachment blob could not be opened", source))?;
    let mut reader = BufReader::new(file);
    let mut hasher = attachment_hash::new_hasher(original_name);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| internal_error("attachment blob could not be read", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn stable_view_path(
    attachment_directory: &Path,
    attachment_id: &AttachmentId,
    original_name: &str,
) -> PathBuf {
    attachment_directory
        .join(attachment_id.as_str())
        .join(safe_display_name(original_name))
}

pub(super) fn relative_blob_link(blob_hash: &str) -> PathBuf {
    PathBuf::from("../../../../")
        .join("blobs")
        .join("sha256")
        .join(&blob_hash[..2])
        .join(blob_hash)
}

/// 创建缺失视图；遇到任何已有未知对象都 fail-closed，不覆盖用户可观察内容。
pub(super) fn ensure_stable_view(view: &Path, blob_hash: &str) -> StorageResult<bool> {
    let expected = relative_blob_link(blob_hash);
    match fs::symlink_metadata(view) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(view)
                .map_err(|source| internal_error("attachment view could not be read", source))?;
            if target != expected {
                return Err(invalid_data("attachment view points to an unexpected blob"));
            }
            Ok(false)
        }
        Ok(_) => Err(invalid_data("attachment view is not a symbolic link")),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let parent = view.parent().expect("attachment view has a parent");
            prepare_private_directory(parent).map_err(|source| {
                internal_error("attachment view directory could not be prepared", source)
            })?;
            symlink(&expected, view)
                .map_err(|source| internal_error("attachment view could not be created", source))?;
            super::sync_directory(parent)?;
            Ok(true)
        }
        Err(source) => Err(internal_error(
            "attachment view could not be inspected",
            source,
        )),
    }
}
