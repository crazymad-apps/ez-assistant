//! 有界批量编辑：预检身份，逐文件内存校验，在核验句柄上提交。

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    path::PathBuf,
    sync::Arc,
};

use agent_tools::{
    AbsolutePath, EditFileEntry, EditFileOutcome, EditFileRequest, EditFileResult,
    EditNotExecutedReason, FileToolContext, FileToolError, exact_replace,
};
use same_file::Handle;

use super::{
    LocalFileSystem, UTF8_BOM, check_cancelled, ensure_unchanged, has_only_crlf_endings,
    map_file_io, to_crlf, validate_text_bytes,
};

// 句柄必须保留到整批结束，避免 Windows 在关闭句柄后复用文件 ID。
struct Identity {
    canonical: PathBuf,
    handle: Handle,
}

impl LocalFileSystem {
    pub(super) async fn edit_batch(
        &self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> Result<EditFileResult, FileToolError> {
        request.validate()?;
        let mut result =
            EditFileResult::not_executed(&request, EditNotExecutedReason::PreflightFailed);
        let mut identities = Vec::with_capacity(request.files.len());
        // 在首个副作用前检查全部对象。任务有界且始终 await，取消不丢弃拥有文件句柄的工作。
        for (index, entry) in request.files.iter().enumerate() {
            if context.cancellation.is_cancelled() {
                return Ok(EditFileResult::not_executed(
                    &request,
                    EditNotExecutedReason::Cancelled,
                ));
            }
            let path = entry.path.clone();
            let identity = tokio::task::spawn_blocking(move || identity(&path)).await;
            let identity = match identity {
                Ok(Ok(identity)) => identity,
                Ok(Err(error)) => {
                    result.files[index].outcome = EditFileOutcome::failed(&error, None);
                    return Ok(result);
                }
                Err(_) => {
                    result.files[index].outcome =
                        EditFileOutcome::failed(&FileToolError::io("identity worker failed"), None);
                    return Ok(result);
                }
            };
            if identities.iter().any(|previous: &Arc<Identity>| {
                previous.canonical == identity.canonical || previous.handle == identity.handle
            }) {
                result.files[index].outcome = EditFileOutcome::Failed {
                    code: "duplicate_target".to_owned(),
                    reason: "two targets refer to the same file".to_owned(),
                    edit_index: None,
                };
                return Ok(result);
            }
            identities.push(Arc::new(identity));
        }
        result = EditFileResult::not_executed(&request, EditNotExecutedReason::PreviousFailure);
        for (index, (entry, expected)) in request
            .files
            .into_iter()
            .zip(identities.iter().cloned())
            .enumerate()
        {
            let lock = self.locks.lock_for(&entry.path);
            let guard = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => { mark_cancelled(&mut result, index); break; }
                guard = lock.lock() => guard,
            };
            if context.cancellation.is_cancelled() {
                mark_cancelled(&mut result, index);
                break;
            }
            let context = context.clone();
            let maximum = self.config.max_text_file_bytes;
            // 同一逻辑路径的锁只保护当前本地提交，不跨宿主回调或另一个文件。
            // 写入启动后不 select/drop，等待 flush 和句柄清理再公布结果。
            let outcome =
                tokio::task::spawn_blocking(move || edit_one(&entry, &expected, maximum, &context))
                    .await;
            drop(guard);
            let outcome = outcome.unwrap_or_else(|_| EditFileOutcome::Unknown {
                code: "worker_failed".to_owned(),
                reason: "file commit could not be confirmed".to_owned(),
            });
            let success = matches!(outcome, EditFileOutcome::Success { .. });
            let cancelled = matches!(
                outcome,
                EditFileOutcome::NotExecuted {
                    reason: EditNotExecutedReason::Cancelled
                }
            );
            result.files[index].outcome = outcome;
            if cancelled {
                mark_cancelled(&mut result, index + 1);
            }
            if !success {
                break;
            }
        }
        Ok(result)
    }
}

fn mark_cancelled(result: &mut EditFileResult, start: usize) {
    for file in result.files.iter_mut().skip(start) {
        file.outcome = EditFileOutcome::NotExecuted {
            reason: EditNotExecutedReason::Cancelled,
        };
    }
}

fn identity(path: &AbsolutePath) -> Result<Identity, FileToolError> {
    if !cfg!(any(unix, windows)) {
        return Err(FileToolError::io(
            "file identity is unsupported on this platform",
        ));
    }
    ensure_regular(path)?;
    let file =
        File::open(path.as_path()).map_err(|error| map_file_io(error, path, "open identity"))?;
    if !file
        .metadata()
        .map_err(|error| map_file_io(error, path, "identity metadata"))?
        .is_file()
    {
        return Err(FileToolError::UnsupportedFileType { path: path.clone() });
    }
    Ok(Identity {
        canonical: std::fs::canonicalize(path.as_path())
            .map_err(|error| map_file_io(error, path, "canonicalize"))?,
        handle: Handle::from_file(file)
            .map_err(|error| map_file_io(error, path, "file identity"))?,
    })
}

fn ensure_regular(path: &AbsolutePath) -> Result<(), FileToolError> {
    // 先拒绝已存在的 FIFO/设备等对象；身份机制不承诺消除外部恶意路径替换竞态。
    if !std::fs::metadata(path.as_path())
        .map_err(|error| map_file_io(error, path, "metadata"))?
        .is_file()
    {
        return Err(FileToolError::UnsupportedFileType { path: path.clone() });
    }
    Ok(())
}

fn check_identity(path: &AbsolutePath, expected: &Identity) -> Result<(), FileToolError> {
    let current = identity(path)?;
    if current.canonical != expected.canonical || current.handle != expected.handle {
        return Err(FileToolError::ConcurrentModification { path: path.clone() });
    }
    Ok(())
}

fn read_bytes(
    file: &mut File,
    path: &AbsolutePath,
    maximum: NonZeroU64,
    context: &FileToolContext,
) -> Result<Vec<u8>, FileToolError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| map_file_io(error, path, "seek"))?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        check_cancelled(context)?;
        let n = file
            .read(&mut chunk)
            .map_err(|error| map_file_io(error, path, "read"))?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > maximum.get() {
            return Err(FileToolError::TooLarge {
                path: path.clone(),
                actual_bytes: bytes.len() as u64 + n as u64,
                maximum_bytes: maximum,
            });
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    validate_text_bytes(path, &bytes, maximum)?;
    Ok(bytes)
}

fn edit_one(
    entry: &EditFileEntry,
    expected: &Identity,
    maximum: NonZeroU64,
    context: &FileToolContext,
) -> EditFileOutcome {
    let prepared = (|| {
        check_cancelled(context)?;
        check_identity(&entry.path, expected)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(entry.path.as_path())
            .map_err(|error| map_file_io(error, &entry.path, "open edit"))?;
        if !file
            .metadata()
            .map_err(|error| map_file_io(error, &entry.path, "edit metadata"))?
            .is_file()
        {
            return Err(FileToolError::UnsupportedFileType {
                path: entry.path.clone(),
            });
        }
        let opened = Handle::from_file(
            file.try_clone()
                .map_err(|error| map_file_io(error, &entry.path, "clone identity"))?,
        )
        .map_err(|error| map_file_io(error, &entry.path, "opened identity"))?;
        if opened != expected.handle {
            return Err(FileToolError::ConcurrentModification {
                path: entry.path.clone(),
            });
        }
        let initial = read_bytes(&mut file, &entry.path, maximum, context)?;
        Ok((file, initial))
    })();
    let (mut file, initial) = match prepared {
        Ok(value) => value,
        Err(FileToolError::Cancelled) => {
            return EditFileOutcome::NotExecuted {
                reason: EditNotExecutedReason::Cancelled,
            };
        }
        Err(error) => return EditFileOutcome::failed(&error, None),
    };
    let replacement = match prepare_edit(entry, &initial, maximum, context) {
        Ok(value) => value,
        Err((FileToolError::Cancelled, _)) => {
            return EditFileOutcome::NotExecuted {
                reason: EditNotExecutedReason::Cancelled,
            };
        }
        Err((error, index)) => return EditFileOutcome::failed(&error, Some(index)),
    };
    let recheck = (|| {
        check_identity(&entry.path, expected)?;
        let current = read_bytes(&mut file, &entry.path, maximum, context)?;
        ensure_unchanged(&entry.path, &initial, &current)?;
        check_cancelled(context)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| map_file_io(error, &entry.path, "seek commit"))
    })();
    if let Err(error) = recheck {
        return if error == FileToolError::Cancelled {
            EditFileOutcome::NotExecuted {
                reason: EditNotExecutedReason::Cancelled,
            }
        } else {
            EditFileOutcome::failed(&error, None)
        };
    }
    commit(&mut file, &replacement.bytes, replacement.replacements)
}

// 从首次截断起失败均无法保证原始内容完整；不回滚、不重试、不伪造完成次数。
fn commit(file: &mut File, bytes: &[u8], replacements: u64) -> EditFileOutcome {
    let truncated = file.set_len(0);
    commit_after_truncate(truncated, file, bytes, replacements)
}

// 私有 Write 边界使部分写入/flush 错误可确定性验证，不向上暴露文件故障注入 SPI。
fn commit_after_truncate(
    truncated: std::io::Result<()>,
    writer: &mut impl Write,
    bytes: &[u8],
    replacements: u64,
) -> EditFileOutcome {
    if truncated
        .and_then(|()| writer.write_all(bytes))
        .and_then(|()| writer.flush())
        .is_err()
    {
        EditFileOutcome::Unknown {
            code: "io".to_owned(),
            reason: "file commit could not be confirmed".to_owned(),
        }
    } else {
        EditFileOutcome::Success { replacements }
    }
}

pub(super) struct PreparedEdit {
    pub(super) bytes: Vec<u8>,
    pub(super) replacements: u64,
}

pub(super) fn prepare_edit(
    entry: &EditFileEntry,
    initial: &[u8],
    maximum: NonZeroU64,
    context: &FileToolContext,
) -> Result<PreparedEdit, (FileToolError, usize)> {
    validate_text_bytes(&entry.path, initial, maximum).map_err(|error| (error, 0))?;
    let (bom, bytes) = initial
        .strip_prefix(UTF8_BOM)
        .map_or((false, initial), |bytes| (true, bytes));
    let mut content = std::str::from_utf8(bytes)
        .map_err(|_| {
            (
                FileToolError::UnsupportedEncoding {
                    path: entry.path.clone(),
                },
                0,
            )
        })?
        .to_owned();
    let crlf = has_only_crlf_endings(&content);
    let mut replacements = 0;
    for (index, edit) in entry.edits.iter().enumerate() {
        check_cancelled(context).map_err(|error| (error, index))?;
        let (old, new) = if crlf {
            (to_crlf(&edit.old_string), to_crlf(&edit.new_string))
        } else {
            (edit.old_string.clone(), edit.new_string.clone())
        };
        // 在分配替换正文前计算长度，避免小 old/大 new 放大为无界内存。
        let matches = content.matches(&old).count();
        if matches > 0 && (edit.replace_all || matches == 1) {
            let length = content
                .len()
                .saturating_sub(matches.saturating_mul(old.len()))
                .saturating_add(matches.saturating_mul(new.len()))
                .saturating_add(if bom { UTF8_BOM.len() } else { 0 });
            if length as u64 > maximum.get() {
                return Err((
                    FileToolError::TooLarge {
                        path: entry.path.clone(),
                        actual_bytes: length as u64,
                        maximum_bytes: maximum,
                    },
                    index,
                ));
            }
        }
        let (next, count) = exact_replace(&content, &old, &new, edit.replace_all)
            .map_err(|error| (error, index))?;
        validate_text_bytes(&entry.path, next.as_bytes(), maximum)
            .map_err(|error| (error, index))?;
        if next.len() as u64 + if bom { UTF8_BOM.len() as u64 } else { 0 } > maximum.get() {
            return Err((
                FileToolError::TooLarge {
                    path: entry.path.clone(),
                    actual_bytes: next.len() as u64 + UTF8_BOM.len() as u64,
                    maximum_bytes: maximum,
                },
                index,
            ));
        }
        content = next;
        replacements += count;
    }
    let mut bytes = if bom { UTF8_BOM.to_vec() } else { Vec::new() };
    bytes.extend_from_slice(content.as_bytes());
    Ok(PreparedEdit {
        bytes,
        replacements,
    })
}

#[cfg(test)]
mod tests;
