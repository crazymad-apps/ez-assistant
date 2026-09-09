//! 数据库对齐后的文件收尾。旧模型只按精确键删除，不解释、导入或转换任何旧模型值。
//!
//! 与数据库事务分开：失败保留已提交数据库和可读配置备份，下次启动只重试未完成收尾。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use assistant_runtime::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource};
use thiserror::Error;
use toml_edit::DocumentMut;

#[derive(Debug, Error)]
pub(crate) enum CleanupError {
    #[error("configuration could not be read safely")]
    Unavailable,
    #[error("configuration has invalid TOML syntax; original file was preserved")]
    InvalidSyntax,
    #[error("configuration changed during upgrade; reload before retrying")]
    Conflict,
    #[error("configuration backup could not be verified")]
    Backup,
    #[error("configuration could not be replaced safely")]
    Replace,
}

/// 调用前数据库版本链已提交且 Host 尚未开放业务；无旧键时不写文件或创建备份。
/// 备份重新读取验证后才通过共享配置源的 revision CAS 提交。CAS 失败不重试、不覆盖外部编辑；
/// 成功返回独立备份路径。此函数不触碰数据库、选择、附件及会话文件。
pub(crate) async fn cleanup(
    source: &dyn RuntimeConfigSource,
    home: &Path,
) -> Result<Option<PathBuf>, CleanupError> {
    let original = match source.load().await {
        ConfigSourceLoad::Missing => return Ok(None),
        ConfigSourceLoad::Document(document) => document,
        ConfigSourceLoad::Unavailable(_) => return Err(CleanupError::Unavailable),
    };
    let Some(candidate) = remove_legacy_keys(original.contents())? else {
        return Ok(None);
    };
    let backup_home = home.to_owned();
    let contents = original.contents().as_bytes().to_vec();
    let backup = tokio::task::spawn_blocking(move || backup_original(&backup_home, &contents))
        .await
        .map_err(|_| CleanupError::Backup)??;
    match source
        .replace(Some(original.revision().to_owned()), candidate.clone())
        .await
    {
        ConfigSourceReplace::Applied(document) if document.contents() == candidate => {
            Ok(Some(backup))
        }
        ConfigSourceReplace::Applied(_) | ConfigSourceReplace::Conflict(_) => {
            Err(CleanupError::Conflict)
        }
        ConfigSourceReplace::Unavailable(_) => Err(CleanupError::Replace),
    }
}

fn remove_legacy_keys(contents: &str) -> Result<Option<String>, CleanupError> {
    let mut document = contents
        .parse::<DocumentMut>()
        .map_err(|_| CleanupError::InvalidSyntax)?;
    let mut changed = document.remove("default_model").is_some();
    changed |= document.remove("models").is_some();
    // TableLike 同时覆盖普通表、内联表和点分键；非表旧值保持原状供新配置诊断。
    if let Some(vision) = document
        .get_mut("agent")
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|agent| agent.get_mut("vision"))
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        changed |= vision.remove("model_key").is_some();
    }
    Ok(changed.then(|| document.to_string()))
}

fn backup_original(home: &Path, contents: &[u8]) -> Result<PathBuf, CleanupError> {
    let root = home.join("backups/configuration");
    crate::config_source::prepare_private_directory(&root).map_err(|_| CleanupError::Backup)?;
    let directory = tempfile::Builder::new()
        .prefix("model-cleanup-")
        .tempdir_in(&root)
        .map_err(|_| CleanupError::Backup)?
        .keep();
    let path = directory.join("config.toml");
    let mut file =
        super::super::create_new_private_file(&path).map_err(|_| CleanupError::Backup)?;
    file.write_all(contents).map_err(|_| CleanupError::Backup)?;
    file.sync_all().map_err(|_| CleanupError::Backup)?;
    if fs::read(&path).map_err(|_| CleanupError::Backup)? != contents {
        return Err(CleanupError::Backup);
    }
    super::super::sync_directory(&directory).map_err(|_| CleanupError::Backup)?;
    super::super::sync_directory(&root).map_err(|_| CleanupError::Backup)?;
    Ok(path)
}

#[cfg(test)]
mod tests;
