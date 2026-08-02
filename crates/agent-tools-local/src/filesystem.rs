//! 真实本地 UTF-8 文件能力实现。

use std::{ffi::OsString, io::ErrorKind, num::NonZeroU64, path::Path};

use agent_tools::{
    AbsolutePath, DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileEntry,
    FileEntryKind, FileSystemTool, FileToolContext, FileToolError, FsFuture, ListDirectoryRequest,
    ListDirectoryResult, ReadFileRequest, ReadFileResult, SearchFilesRequest, SearchFilesResult,
    WriteFileRequest, WriteFileResult, exact_replace, paginate_with_line_numbers,
};

use crate::{path_lock::PathLockTable, search};

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// 本地文件能力的实例级资源限制和搜索后端配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFileSystemConfig {
    /// read/write/edit 接受的最大文本文件字节数。
    pub max_text_file_bytes: NonZeroU64,
    /// 直接启动的 ripgrep 程序名或路径，不经 Shell。
    pub ripgrep_program: OsString,
    /// ripgrep stderr 允许保留的最大字节数；超出部分继续排空但不累计。
    pub max_search_stderr_bytes: NonZeroU64,
}

/// 真实本地文件系统 Adapter；不包含权限、审批或审计规则。
pub struct LocalFileSystem {
    config: LocalFileSystemConfig,
    locks: PathLockTable,
}

impl LocalFileSystem {
    /// 用显式文件大小上限和 ripgrep 程序创建 Adapter。
    pub fn new(config: LocalFileSystemConfig) -> Self {
        Self {
            config,
            locks: PathLockTable::default(),
        }
    }

    /// 返回当前实例冻结的配置。
    pub fn config(&self) -> &LocalFileSystemConfig {
        &self.config
    }

    async fn read_text_bytes(&self, path: &AbsolutePath) -> Result<Vec<u8>, FileToolError> {
        let metadata = tokio::fs::metadata(path.as_path())
            .await
            .map_err(|error| map_file_io(error, path, "read metadata"))?;
        if !metadata.is_file() {
            return Err(FileToolError::UnsupportedFileType { path: path.clone() });
        }
        if metadata.len() > self.config.max_text_file_bytes.get() {
            return Err(FileToolError::TooLarge {
                path: path.clone(),
                actual_bytes: metadata.len(),
                maximum_bytes: self.config.max_text_file_bytes,
            });
        }
        let bytes = tokio::fs::read(path.as_path())
            .await
            .map_err(|error| map_file_io(error, path, "read"))?;
        validate_text_bytes(path, &bytes, self.config.max_text_file_bytes)?;
        Ok(bytes)
    }

    async fn write_locked(
        &self,
        request: WriteFileRequest,
        context: FileToolContext,
    ) -> Result<WriteFileResult, FileToolError> {
        validate_text_bytes(
            &request.path,
            request.content.as_bytes(),
            self.config.max_text_file_bytes,
        )?;
        let parent = request.path.as_path().parent().ok_or_else(|| {
            FileToolError::invalid_input("write target must have a parent directory")
        })?;
        let parent_path = AbsolutePath::new(parent.to_path_buf())
            .map_err(|error| FileToolError::invalid_input(error.to_string()))?;
        let parent_metadata = tokio::fs::metadata(parent)
            .await
            .map_err(|error| map_file_io(error, &parent_path, "read parent metadata"))?;
        if !parent_metadata.is_dir() {
            return Err(FileToolError::UnsupportedFileType { path: parent_path });
        }
        match tokio::fs::metadata(request.path.as_path()).await {
            Ok(metadata) if !metadata.is_file() => {
                return Err(FileToolError::UnsupportedFileType { path: request.path });
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(map_file_io(error, &request.path, "read target metadata")),
        }
        check_cancelled(&context)?;
        let bytes_written = request.content.len() as u64;
        tokio::fs::write(request.path.as_path(), request.content.as_bytes())
            .await
            .map_err(|error| map_file_io(error, &request.path, "write"))?;
        Ok(WriteFileResult {
            path: request.path,
            bytes_written,
        })
    }

    async fn edit_locked(
        &self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> Result<EditFileResult, FileToolError> {
        let initial = self.read_text_bytes(&request.path).await?;
        let replacement = prepare_edit(&request, &initial, self.config.max_text_file_bytes)?;
        let current = self.read_text_bytes(&request.path).await?;
        ensure_unchanged(&request.path, &initial, &current)?;
        check_cancelled(&context)?;
        tokio::fs::write(request.path.as_path(), &replacement.bytes)
            .await
            .map_err(|error| map_file_io(error, &request.path, "edit write"))?;
        Ok(EditFileResult {
            path: request.path,
            replacements: replacement.replacements,
        })
    }

    async fn delete_locked(
        &self,
        request: DeleteFileRequest,
        context: FileToolContext,
    ) -> Result<DeleteFileResult, FileToolError> {
        let metadata = tokio::fs::symlink_metadata(request.path.as_path())
            .await
            .map_err(|error| map_file_io(error, &request.path, "read delete metadata"))?;
        check_cancelled(&context)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            remove_symlink(request.path.as_path(), &request.path).await?;
        } else if metadata.is_file() {
            tokio::fs::remove_file(request.path.as_path())
                .await
                .map_err(|error| map_file_io(error, &request.path, "delete file"))?;
        } else if metadata.is_dir() {
            tokio::fs::remove_dir(request.path.as_path())
                .await
                .map_err(|error| map_file_io(error, &request.path, "delete empty directory"))?;
        } else {
            return Err(FileToolError::UnsupportedFileType { path: request.path });
        }
        Ok(DeleteFileResult {
            deleted: request.path,
        })
    }
}

impl FileSystemTool for LocalFileSystem {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult> {
        Box::pin(async move {
            check_cancelled(&context)?;
            let bytes = self.read_text_bytes(&request.path).await?;
            check_cancelled(&context)?;
            let content_bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(&bytes);
            let text = std::str::from_utf8(content_bytes).map_err(|_| {
                FileToolError::UnsupportedEncoding {
                    path: request.path.clone(),
                }
            })?;
            let (content, next_offset, truncated) =
                paginate_with_line_numbers(text, request.offset, request.limit);
            Ok(ReadFileResult {
                path: request.path,
                offset: request.offset,
                limit: request.limit,
                content,
                next_offset,
                truncated,
            })
        })
    }

    fn list<'a>(
        &'a self,
        request: ListDirectoryRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ListDirectoryResult> {
        Box::pin(async move {
            check_cancelled(&context)?;
            let metadata = tokio::fs::metadata(request.path.as_path())
                .await
                .map_err(|error| map_file_io(error, &request.path, "read directory metadata"))?;
            if !metadata.is_dir() {
                return Err(FileToolError::UnsupportedFileType { path: request.path });
            }
            let mut directory = tokio::fs::read_dir(request.path.as_path())
                .await
                .map_err(|error| map_file_io(error, &request.path, "list directory"))?;
            let mut entries = Vec::new();
            while let Some(entry) = directory
                .next_entry()
                .await
                .map_err(|error| map_file_io(error, &request.path, "read directory entry"))?
            {
                check_cancelled(&context)?;
                let path = AbsolutePath::new(entry.path()).map_err(|error| {
                    FileToolError::io(format!("directory entry path is invalid: {error}"))
                })?;
                let link_metadata = tokio::fs::symlink_metadata(path.as_path())
                    .await
                    .map_err(|error| map_file_io(error, &path, "read entry metadata"))?;
                let is_symlink = link_metadata.file_type().is_symlink();
                let kind = if is_symlink {
                    match tokio::fs::metadata(path.as_path()).await {
                        Ok(metadata) => classify_metadata(&metadata),
                        Err(_) => FileEntryKind::Other,
                    }
                } else {
                    classify_metadata(&link_metadata)
                };
                entries.push(FileEntry {
                    path,
                    kind,
                    is_symlink,
                });
            }
            entries.sort_by(|left, right| left.path.cmp(&right.path));
            Ok(ListDirectoryResult { entries })
        })
    }

    fn search<'a>(
        &'a self,
        request: SearchFilesRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            search::run_with_stderr_limit(
                &self.config.ripgrep_program,
                request,
                self.config.max_search_stderr_bytes,
                context.cancellation,
            )
            .await
        })
    }

    fn write<'a>(
        &'a self,
        request: WriteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, WriteFileResult> {
        Box::pin(async move {
            check_cancelled(&context)?;
            let lock = self.locks.lock_for(&request.path);
            let _guard = tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => return Err(FileToolError::Cancelled),
                guard = lock.lock() => guard,
            };
            check_cancelled(&context)?;
            self.write_locked(request, context).await
        })
    }

    fn delete<'a>(
        &'a self,
        request: DeleteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            check_cancelled(&context)?;
            let lock = self.locks.lock_for(&request.path);
            let _guard = tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => return Err(FileToolError::Cancelled),
                guard = lock.lock() => guard,
            };
            check_cancelled(&context)?;
            self.delete_locked(request, context).await
        })
    }

    fn edit<'a>(
        &'a self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, EditFileResult> {
        Box::pin(async move {
            check_cancelled(&context)?;
            let lock = self.locks.lock_for(&request.path);
            let _guard = tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => return Err(FileToolError::Cancelled),
                guard = lock.lock() => guard,
            };
            check_cancelled(&context)?;
            self.edit_locked(request, context).await
        })
    }
}

struct PreparedEdit {
    bytes: Vec<u8>,
    replacements: u64,
}

fn prepare_edit(
    request: &EditFileRequest,
    initial: &[u8],
    maximum_bytes: NonZeroU64,
) -> Result<PreparedEdit, FileToolError> {
    validate_text_bytes(&request.path, initial, maximum_bytes)?;
    let (has_bom, content_bytes) = initial
        .strip_prefix(UTF8_BOM)
        .map_or((false, initial), |content| (true, content));
    let content =
        std::str::from_utf8(content_bytes).map_err(|_| FileToolError::UnsupportedEncoding {
            path: request.path.clone(),
        })?;
    let (old_string, new_string) = if has_only_crlf_endings(content) {
        (to_crlf(&request.old_string), to_crlf(&request.new_string))
    } else {
        (request.old_string.clone(), request.new_string.clone())
    };
    let (edited, replacements) =
        exact_replace(content, &old_string, &new_string, request.replace_all)?;
    let mut bytes = Vec::with_capacity(edited.len() + usize::from(has_bom) * UTF8_BOM.len());
    if has_bom {
        bytes.extend_from_slice(UTF8_BOM);
    }
    bytes.extend_from_slice(edited.as_bytes());
    validate_text_bytes(&request.path, &bytes, maximum_bytes)?;
    Ok(PreparedEdit {
        bytes,
        replacements,
    })
}

fn validate_text_bytes(
    path: &AbsolutePath,
    bytes: &[u8],
    maximum_bytes: NonZeroU64,
) -> Result<(), FileToolError> {
    let actual_bytes = bytes.len() as u64;
    if actual_bytes > maximum_bytes.get() {
        return Err(FileToolError::TooLarge {
            path: path.clone(),
            actual_bytes,
            maximum_bytes,
        });
    }
    if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        return Err(FileToolError::UnsupportedEncoding { path: path.clone() });
    }
    Ok(())
}

fn ensure_unchanged(
    path: &AbsolutePath,
    initial: &[u8],
    current: &[u8],
) -> Result<(), FileToolError> {
    if current == initial {
        Ok(())
    } else {
        Err(FileToolError::ConcurrentModification { path: path.clone() })
    }
}

fn has_only_crlf_endings(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut has_newline = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        match byte {
            b'\n' => {
                has_newline = true;
                if index == 0 || bytes[index - 1] != b'\r' {
                    return false;
                }
            }
            b'\r' if bytes.get(index + 1) != Some(&b'\n') => return false,
            _ => {}
        }
    }
    has_newline
}

fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn classify_metadata(metadata: &std::fs::Metadata) -> FileEntryKind {
    if metadata.is_file() {
        FileEntryKind::File
    } else if metadata.is_dir() {
        FileEntryKind::Directory
    } else {
        FileEntryKind::Other
    }
}

fn check_cancelled(context: &FileToolContext) -> Result<(), FileToolError> {
    if context.cancellation.is_cancelled() {
        Err(FileToolError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_file_io(error: std::io::Error, path: &AbsolutePath, operation: &str) -> FileToolError {
    if error.kind() == ErrorKind::NotFound {
        FileToolError::NotFound { path: path.clone() }
    } else {
        FileToolError::io(format!("{operation} `{path}` failed: {error}"))
    }
}

#[cfg(unix)]
async fn remove_symlink(path: &Path, logical_path: &AbsolutePath) -> Result<(), FileToolError> {
    tokio::fs::remove_file(path)
        .await
        .map_err(|error| map_file_io(error, logical_path, "delete symlink"))
}

#[cfg(windows)]
async fn remove_symlink(path: &Path, logical_path: &AbsolutePath) -> Result<(), FileToolError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(file_error) => tokio::fs::remove_dir(path)
            .await
            .map_err(|directory_error| {
                FileToolError::io(format!(
                    "delete symlink `{logical_path}` failed as file ({file_error}) and directory \
                     ({directory_error})"
                ))
            }),
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::Arc};

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn fixture(maximum_bytes: u64) -> (TempDir, Arc<LocalFileSystem>) {
        let directory = tempfile::tempdir().expect("temp directory");
        let filesystem = Arc::new(LocalFileSystem::new(LocalFileSystemConfig {
            max_text_file_bytes: NonZeroU64::new(maximum_bytes).expect("non-zero"),
            ripgrep_program: OsString::from("rg"),
            max_search_stderr_bytes: NonZeroU64::new(64 * 1024).expect("non-zero"),
        }));
        (directory, filesystem)
    }

    fn absolute(path: impl AsRef<Path>) -> AbsolutePath {
        AbsolutePath::new(path.as_ref().to_path_buf()).expect("absolute UTF-8 temp path")
    }

    fn nonzero(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero")
    }

    #[tokio::test]
    async fn filesystem_read_paginates_empty_bom_encoding_type_and_size() {
        let (directory, filesystem) = fixture(16);
        let text_path = directory.path().join("text.txt");
        tokio::fs::write(&text_path, b"one\ntwo\nthree\n")
            .await
            .expect("write fixture");
        let result = filesystem
            .read(
                ReadFileRequest {
                    path: absolute(&text_path),
                    offset: nonzero(2),
                    limit: nonzero(1),
                },
                FileToolContext::default(),
            )
            .await
            .expect("read text");
        assert_eq!(result.content, "2\ttwo");
        assert_eq!(result.next_offset, NonZeroU32::new(3));

        let empty_path = directory.path().join("empty.txt");
        tokio::fs::write(&empty_path, [])
            .await
            .expect("write empty");
        let result = filesystem
            .read(
                ReadFileRequest {
                    path: absolute(&empty_path),
                    offset: nonzero(1),
                    limit: nonzero(1),
                },
                FileToolContext::default(),
            )
            .await
            .expect("read empty");
        assert_eq!(result.content, "");

        let bom_path = directory.path().join("bom.txt");
        tokio::fs::write(&bom_path, b"\xEF\xBB\xBFhello")
            .await
            .expect("write bom");
        let result = filesystem
            .read(
                ReadFileRequest {
                    path: absolute(&bom_path),
                    offset: nonzero(1),
                    limit: nonzero(1),
                },
                FileToolContext::default(),
            )
            .await
            .expect("read bom");
        assert_eq!(result.content, "1\thello");

        let invalid_path = directory.path().join("invalid.txt");
        tokio::fs::write(&invalid_path, [0xff])
            .await
            .expect("write invalid");
        assert!(matches!(
            filesystem
                .read(
                    ReadFileRequest {
                        path: absolute(&invalid_path),
                        offset: nonzero(1),
                        limit: nonzero(1),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedEncoding { .. })
        ));

        let nul_path = directory.path().join("nul.txt");
        tokio::fs::write(&nul_path, b"text\0data")
            .await
            .expect("write NUL fixture");
        assert!(matches!(
            filesystem
                .read(
                    ReadFileRequest {
                        path: absolute(&nul_path),
                        offset: nonzero(1),
                        limit: nonzero(1),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedEncoding { .. })
        ));

        assert!(matches!(
            filesystem
                .read(
                    ReadFileRequest {
                        path: absolute(directory.path()),
                        offset: nonzero(1),
                        limit: nonzero(1),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedFileType { .. })
        ));

        let large_path = directory.path().join("large.txt");
        tokio::fs::write(&large_path, b"0123456789abcdefg")
            .await
            .expect("write large");
        assert!(matches!(
            filesystem
                .read(
                    ReadFileRequest {
                        path: absolute(&large_path),
                        offset: nonzero(1),
                        limit: nonzero(1),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::TooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn filesystem_write_edit_delete_and_cancel_follow_contract() {
        let (directory, filesystem) = fixture(1024);
        let path = directory.path().join("file.txt");
        filesystem
            .write(
                WriteFileRequest {
                    path: absolute(&path),
                    content: "one\ntwo\n".to_owned(),
                },
                FileToolContext::default(),
            )
            .await
            .expect("create file");
        filesystem
            .edit(
                EditFileRequest {
                    path: absolute(&path),
                    old_string: "one".to_owned(),
                    new_string: "first".to_owned(),
                    replace_all: false,
                },
                FileToolContext::default(),
            )
            .await
            .expect("edit file");
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("read edited"),
            "first\ntwo\n"
        );

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = filesystem
            .write(
                WriteFileRequest {
                    path: absolute(&path),
                    content: "cancelled".to_owned(),
                },
                FileToolContext::new(cancellation),
            )
            .await
            .expect_err("cancel before side effect");
        assert_eq!(error, FileToolError::Cancelled);
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .expect("read unchanged"),
            "first\ntwo\n"
        );

        assert!(matches!(
            filesystem
                .write(
                    WriteFileRequest {
                        path: absolute(&path),
                        content: "invalid\0text".to_owned(),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedEncoding { .. })
        ));

        filesystem
            .delete(
                DeleteFileRequest {
                    path: absolute(&path),
                },
                FileToolContext::default(),
            )
            .await
            .expect("delete file");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn filesystem_edit_preserves_bom_crlf_and_serializes_same_path() {
        let (directory, filesystem) = fixture(1024);
        let path = directory.path().join("file.txt");
        tokio::fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\r\n")
            .await
            .expect("write fixture");
        filesystem
            .edit(
                EditFileRequest {
                    path: absolute(&path),
                    old_string: "one\ntwo".to_owned(),
                    new_string: "first\nsecond".to_owned(),
                    replace_all: false,
                },
                FileToolContext::default(),
            )
            .await
            .expect("edit CRLF");
        assert_eq!(
            tokio::fs::read(&path).await.expect("read edited"),
            b"\xEF\xBB\xBFfirst\r\nsecond\r\n"
        );

        tokio::fs::write(&path, "x x").await.expect("reset fixture");
        let first = filesystem.edit(
            EditFileRequest {
                path: absolute(&path),
                old_string: "x".to_owned(),
                new_string: "y".to_owned(),
                replace_all: true,
            },
            FileToolContext::default(),
        );
        let second = filesystem.edit(
            EditFileRequest {
                path: absolute(&path),
                old_string: "y".to_owned(),
                new_string: "z".to_owned(),
                replace_all: true,
            },
            FileToolContext::default(),
        );
        let (first, second) = tokio::join!(first, second);
        first.expect("first edit");
        second.expect("second edit waits for first");
        assert_eq!(tokio::fs::read_to_string(&path).await.expect("read"), "z z");
    }

    #[tokio::test]
    async fn mutation_waiting_for_same_path_lock_responds_to_cancellation() {
        let (directory, filesystem) = fixture(1024);
        let path = directory.path().join("file.txt");
        tokio::fs::write(&path, "original")
            .await
            .expect("write fixture");
        let absolute_path = absolute(&path);
        let lock = filesystem.locks.lock_for(&absolute_path);
        let _guard = lock.lock().await;

        let cancellation = CancellationToken::new();
        let cancellation_trigger = cancellation.clone();
        let trigger = tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancellation_trigger.cancel();
        });
        let context = || FileToolContext::new(cancellation.clone());
        let results = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                filesystem.write(
                    WriteFileRequest {
                        path: absolute_path.clone(),
                        content: "changed".to_owned(),
                    },
                    context(),
                ),
                filesystem.edit(
                    EditFileRequest {
                        path: absolute_path.clone(),
                        old_string: "original".to_owned(),
                        new_string: "changed".to_owned(),
                        replace_all: false,
                    },
                    context(),
                ),
                filesystem.delete(
                    DeleteFileRequest {
                        path: absolute_path,
                    },
                    context(),
                ),
            )
        })
        .await
        .expect("cancelled lock waiters settle promptly");
        trigger.await.expect("cancellation trigger");
        assert_eq!(results.0, Err(FileToolError::Cancelled));
        assert_eq!(results.1, Err(FileToolError::Cancelled));
        assert_eq!(results.2, Err(FileToolError::Cancelled));
        assert_eq!(
            tokio::fs::read_to_string(&path)
                .await
                .expect("read unchanged"),
            "original"
        );
    }

    #[tokio::test]
    async fn filesystem_list_reports_symlink_orthogonally_and_delete_unlinks_it() {
        let (directory, filesystem) = fixture(1024);
        let file = directory.path().join("file.txt");
        let subdirectory = directory.path().join("subdirectory");
        tokio::fs::write(&file, "content")
            .await
            .expect("write file");
        tokio::fs::create_dir(&subdirectory)
            .await
            .expect("create directory");
        let result = filesystem
            .list(
                ListDirectoryRequest {
                    path: absolute(directory.path()),
                },
                FileToolContext::default(),
            )
            .await
            .expect("list");
        let file_entry = result
            .entries
            .iter()
            .find(|entry| entry.path == absolute(&file))
            .expect("file entry");
        assert_eq!(file_entry.kind, FileEntryKind::File);
        assert!(!file_entry.is_symlink);
        let directory_entry = result
            .entries
            .iter()
            .find(|entry| entry.path == absolute(&subdirectory))
            .expect("directory entry");
        assert_eq!(directory_entry.kind, FileEntryKind::Directory);
        assert!(!directory_entry.is_symlink);

        #[cfg(unix)]
        {
            let file_link = directory.path().join("file-link");
            let directory_link = directory.path().join("directory-link");
            std::os::unix::fs::symlink(&file, &file_link).expect("file symlink");
            std::os::unix::fs::symlink(&subdirectory, &directory_link).expect("directory symlink");
            let symlink_result = filesystem
                .list(
                    ListDirectoryRequest {
                        path: absolute(directory.path()),
                    },
                    FileToolContext::default(),
                )
                .await
                .expect("list");
            let file_entry = symlink_result
                .entries
                .iter()
                .find(|entry| entry.path == absolute(&file_link))
                .expect("file link entry");
            assert_eq!(file_entry.kind, FileEntryKind::File);
            assert!(file_entry.is_symlink);
            let directory_entry = symlink_result
                .entries
                .iter()
                .find(|entry| entry.path == absolute(&directory_link))
                .expect("directory link entry");
            assert_eq!(directory_entry.kind, FileEntryKind::Directory);
            assert!(directory_entry.is_symlink);

            filesystem
                .delete(
                    DeleteFileRequest {
                        path: absolute(&file_link),
                    },
                    FileToolContext::default(),
                )
                .await
                .expect("delete symlink");
            assert!(!file_link.exists());
            assert!(file.exists());

            let write_link = directory.path().join("write-link");
            std::os::unix::fs::symlink(&file, &write_link).expect("write symlink");
            filesystem
                .write(
                    WriteFileRequest {
                        path: absolute(&write_link),
                        content: "written through link".to_owned(),
                    },
                    FileToolContext::default(),
                )
                .await
                .expect("write through symlink");
            filesystem
                .edit(
                    EditFileRequest {
                        path: absolute(&write_link),
                        old_string: "written".to_owned(),
                        new_string: "edited".to_owned(),
                        replace_all: false,
                    },
                    FileToolContext::default(),
                )
                .await
                .expect("edit through symlink");
            assert_eq!(
                tokio::fs::read_to_string(&file).await.expect("read target"),
                "edited through link"
            );
        }
    }

    #[tokio::test]
    async fn filesystem_delete_only_accepts_empty_directories() {
        let (directory, filesystem) = fixture(1024);
        let empty = directory.path().join("empty");
        tokio::fs::create_dir(&empty).await.expect("create empty");
        filesystem
            .delete(
                DeleteFileRequest {
                    path: absolute(&empty),
                },
                FileToolContext::default(),
            )
            .await
            .expect("delete empty directory");
        assert!(!empty.exists());

        let nonempty = directory.path().join("nonempty");
        tokio::fs::create_dir(&nonempty)
            .await
            .expect("create nonempty");
        tokio::fs::write(nonempty.join("child"), "content")
            .await
            .expect("write child");
        assert!(matches!(
            filesystem
                .delete(
                    DeleteFileRequest {
                        path: absolute(&nonempty),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::Io { .. })
        ));
        assert!(nonempty.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_rejects_special_files_and_lists_them_as_other() {
        use std::os::unix::net::UnixListener;

        let (directory, filesystem) = fixture(1024);
        let socket = directory.path().join("service.sock");
        let _listener = UnixListener::bind(&socket).expect("bind fixture socket");
        let result = filesystem
            .list(
                ListDirectoryRequest {
                    path: absolute(directory.path()),
                },
                FileToolContext::default(),
            )
            .await
            .expect("list special file");
        let entry = result
            .entries
            .iter()
            .find(|entry| entry.path == absolute(&socket))
            .expect("socket entry");
        assert_eq!(entry.kind, FileEntryKind::Other);
        assert!(!entry.is_symlink);
        assert!(matches!(
            filesystem
                .read(
                    ReadFileRequest {
                        path: absolute(&socket),
                        offset: nonzero(1),
                        limit: nonzero(1),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedFileType { .. })
        ));
        assert!(matches!(
            filesystem
                .delete(
                    DeleteFileRequest {
                        path: absolute(&socket),
                    },
                    FileToolContext::default(),
                )
                .await,
            Err(FileToolError::UnsupportedFileType { .. })
        ));
        assert!(socket.exists());
    }

    #[test]
    fn edit_detects_changed_snapshot_and_rejects_mixed_invalid_replacements() {
        let path = absolute(std::env::temp_dir().join("edit-test.txt"));
        let request = EditFileRequest {
            path: path.clone(),
            old_string: "missing".to_owned(),
            new_string: "new".to_owned(),
            replace_all: false,
        };
        assert!(matches!(
            prepare_edit(
                &request,
                b"content",
                NonZeroU64::new(100).expect("non-zero")
            ),
            Err(FileToolError::InvalidInput { .. })
        ));
        assert_eq!(
            ensure_unchanged(&path, b"before", b"after"),
            Err(FileToolError::ConcurrentModification { path })
        );
    }
}
