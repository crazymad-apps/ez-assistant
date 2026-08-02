//! 内存文件树 Fake：执行 [`FileSystemTool`] 的绝对路径与取消契约。
//!
//! 目录由文件路径隐式表达，空目录、符号链接、特殊文件和具体操作系统 errno 不可表达；
//! Fake 不接触真实文件系统。调用方提供 Session 工作目录，初始相对路径按与标准工具
//! 壳相同的词法规则解析。因此搜索不存在的根、删除目录等 backend 相关边界不应被用来
//! 断言真实 Adapter 的精确错误类型。

use std::{collections::BTreeMap, num::NonZeroU32, path::Path, sync::Mutex};

use agent_tools::{
    AbsolutePath, DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileEntry,
    FileEntryKind, FileSystemTool, FileToolContext, FileToolError, FsFuture, ListDirectoryRequest,
    ListDirectoryResult, PathResolutionError, ReadFileRequest, ReadFileResult, SearchFilesRequest,
    SearchFilesResult, SearchKind, SearchMatch, SearchTruncationReason, SessionPathResolver,
    WriteFileRequest, WriteFileResult, exact_replace, paginate_with_line_numbers,
};

/// 内存文件树 Fake；保存和返回的路径始终是绝对逻辑路径。
pub struct FakeFileSystemTool {
    files: Mutex<BTreeMap<AbsolutePath, String>>,
    next_error: Mutex<Option<FileToolError>>,
}

impl FakeFileSystemTool {
    /// 用 Session 工作目录和一组绝对或相对初始文件创建 Fake。
    pub fn new<'a>(
        session_workdir: AbsolutePath,
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, PathResolutionError> {
        let resolver = SessionPathResolver::new(session_workdir);
        let files = files
            .into_iter()
            .map(|(path, content)| Ok((resolver.resolve(path)?, content.to_owned())))
            .collect::<Result<_, PathResolutionError>>()?;
        Ok(Self {
            files: Mutex::new(files),
            next_error: Mutex::new(None),
        })
    }

    /// 让下一次能力调用返回指定错误，用于覆盖 I/O、编码和后端失败等分支。
    pub fn fail_next(&self, error: FileToolError) {
        *self.next_error.lock().expect("lock next error") = Some(error);
    }

    /// 读取当前文件内容（断言用）。
    pub fn read_content(&self, path: &AbsolutePath) -> Option<String> {
        self.files.lock().expect("lock files").get(path).cloned()
    }

    /// 当前文件树快照（断言用）。
    pub fn snapshot(&self) -> BTreeMap<AbsolutePath, String> {
        self.files.lock().expect("lock files").clone()
    }

    fn check(&self, context: &FileToolContext) -> Result<(), FileToolError> {
        if context.cancellation.is_cancelled() {
            return Err(FileToolError::Cancelled);
        }
        if let Some(error) = self.next_error.lock().expect("lock next error").take() {
            return Err(error);
        }
        Ok(())
    }

    fn dir_exists(files: &BTreeMap<AbsolutePath, String>, path: &AbsolutePath) -> bool {
        files
            .keys()
            .any(|file| file.as_path().starts_with(path.as_path()) && file != path)
    }
}

impl FileSystemTool for FakeFileSystemTool {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult> {
        Box::pin(async move {
            self.check(&context)?;
            let files = self.files.lock().expect("lock files");
            if Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::UnsupportedFileType { path: request.path });
            }
            let Some(content) = files.get(&request.path) else {
                return Err(FileToolError::NotFound { path: request.path });
            };
            let (content, next_offset, truncated) =
                paginate_with_line_numbers(content, request.offset, request.limit);
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
            self.check(&context)?;
            let files = self.files.lock().expect("lock files");
            if files.contains_key(&request.path) {
                return Err(FileToolError::UnsupportedFileType { path: request.path });
            }
            if !Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::NotFound { path: request.path });
            }
            let mut entries = BTreeMap::<AbsolutePath, FileEntryKind>::new();
            for path in files.keys() {
                let Ok(relative) = path.as_path().strip_prefix(request.path.as_path()) else {
                    continue;
                };
                let mut components = relative.components();
                let Some(first) = components.next() else {
                    continue;
                };
                let entry_path = AbsolutePath::new(request.path.as_path().join(first.as_os_str()))
                    .expect("child of absolute path is absolute");
                let kind = if components.next().is_some() {
                    FileEntryKind::Directory
                } else {
                    FileEntryKind::File
                };
                entries
                    .entry(entry_path)
                    .and_modify(|stored| {
                        if kind == FileEntryKind::Directory {
                            *stored = kind;
                        }
                    })
                    .or_insert(kind);
            }
            Ok(ListDirectoryResult {
                entries: entries
                    .into_iter()
                    .map(|(path, kind)| FileEntry {
                        path,
                        kind,
                        is_symlink: false,
                    })
                    .collect(),
            })
        })
    }

    fn search<'a>(
        &'a self,
        request: SearchFilesRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            self.check(&context)?;
            if request.query.is_empty() {
                return Err(FileToolError::invalid_input("query must not be empty"));
            }
            let files = self.files.lock().expect("lock files");
            let mut matches = Vec::new();
            for (path, content) in files.iter() {
                if !path.as_path().starts_with(request.path.as_path()) {
                    continue;
                }
                match request.kind {
                    SearchKind::ByName => {
                        if path
                            .as_path()
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.contains(&request.query))
                        {
                            matches.push(SearchMatch::Name { path: path.clone() });
                        }
                    }
                    SearchKind::ByContent => {
                        for (index, line) in content.lines().enumerate() {
                            if line.contains(&request.query) {
                                let line_number = u32::try_from(index + 1)
                                    .ok()
                                    .and_then(NonZeroU32::new)
                                    .ok_or_else(|| {
                                        FileToolError::invalid_input("line number exceeds u32")
                                    })?;
                                matches.push(SearchMatch::Content {
                                    path: path.clone(),
                                    line_number,
                                    line: line.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            let limit = request.max_results.get() as usize;
            let truncated = matches.len() > limit;
            matches.truncate(limit);
            Ok(SearchFilesResult {
                matches,
                truncated,
                truncation_reason: truncated.then_some(SearchTruncationReason::MaxResults),
            })
        })
    }

    fn write<'a>(
        &'a self,
        request: WriteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, WriteFileResult> {
        Box::pin(async move {
            self.check(&context)?;
            let mut files = self.files.lock().expect("lock files");
            let parent = request
                .path
                .as_path()
                .parent()
                .unwrap_or_else(|| Path::new("/"));
            let parent = AbsolutePath::new(parent.to_path_buf())
                .expect("parent of absolute path is absolute");
            if !Self::dir_exists(&files, &parent) && parent.as_path() != Path::new("/") {
                return Err(FileToolError::NotFound { path: parent });
            }
            if Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::UnsupportedFileType { path: request.path });
            }
            let bytes_written = request.content.len() as u64;
            let path = request.path;
            files.insert(path.clone(), request.content);
            Ok(WriteFileResult {
                path,
                bytes_written,
            })
        })
    }

    fn delete<'a>(
        &'a self,
        request: DeleteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            self.check(&context)?;
            let mut files = self.files.lock().expect("lock files");
            if files.remove(&request.path).is_some() {
                return Ok(DeleteFileResult {
                    deleted: request.path,
                });
            }
            if Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::invalid_input("directory is not empty"));
            }
            Err(FileToolError::NotFound { path: request.path })
        })
    }

    fn edit<'a>(
        &'a self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, EditFileResult> {
        Box::pin(async move {
            self.check(&context)?;
            let mut files = self.files.lock().expect("lock files");
            let Some(content) = files.get(&request.path).cloned() else {
                return Err(FileToolError::NotFound {
                    path: request.path.clone(),
                });
            };
            let (replaced, replacements) = exact_replace(
                &content,
                &request.old_string,
                &request.new_string,
                request.replace_all,
            )?;
            let path = request.path;
            files.insert(path.clone(), replaced);
            Ok(EditFileResult { path, replacements })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        num::NonZeroU32,
        task::{Context, Poll, Waker},
    };

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    fn root() -> AbsolutePath {
        AbsolutePath::new(std::env::temp_dir().join("agent-testkit-fs"))
            .expect("absolute temp path")
    }

    fn path(relative: &str) -> AbsolutePath {
        SessionPathResolver::new(root())
            .resolve(relative)
            .expect("valid relative path")
    }

    fn nonzero(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero")
    }

    fn fake() -> FakeFileSystemTool {
        FakeFileSystemTool::new(
            root(),
            [
                ("src/main.rs", "fn main() {}\nfn helper() {}\n"),
                ("src/lib.rs", "pub mod helper;\n"),
            ],
        )
        .expect("fake file tree")
    }

    #[test]
    fn read_paginates_and_reports_next_offset() {
        let result = block_on(fake().read(
            ReadFileRequest {
                path: path("src/main.rs"),
                offset: nonzero(1),
                limit: nonzero(1),
            },
            FileToolContext::default(),
        ))
        .expect("read succeeds");
        assert_eq!(result.content, "1\tfn main() {}");
        assert_eq!(result.next_offset, NonZeroU32::new(2));
        assert!(result.truncated);
    }

    #[test]
    fn list_and_search_return_absolute_paths_and_truncation() {
        let listed = block_on(fake().list(
            ListDirectoryRequest { path: root() },
            FileToolContext::default(),
        ))
        .expect("list succeeds");
        assert_eq!(listed.entries[0].path, path("src"));
        assert_eq!(listed.entries[0].kind, FileEntryKind::Directory);
        assert!(!listed.entries[0].is_symlink);

        let by_content = block_on(fake().search(
            SearchFilesRequest {
                query: "helper".to_owned(),
                path: path("src"),
                kind: SearchKind::ByContent,
                max_results: nonzero(1),
                max_output_bytes: std::num::NonZeroU64::new(1024 * 1024).expect("non-zero"),
                max_record_bytes: std::num::NonZeroU64::new(64 * 1024).expect("non-zero"),
            },
            FileToolContext::default(),
        ))
        .expect("search succeeds");
        assert_eq!(by_content.matches.len(), 1);
        assert!(by_content.truncated);
        assert_eq!(
            by_content.truncation_reason,
            Some(SearchTruncationReason::MaxResults)
        );
    }

    #[test]
    fn mutations_errors_and_cancellation_are_observable() {
        let fake = fake();
        block_on(fake.edit(
            EditFileRequest {
                path: path("src/lib.rs"),
                old_string: "helper".to_owned(),
                new_string: "utils".to_owned(),
                replace_all: false,
            },
            FileToolContext::default(),
        ))
        .expect("edit succeeds");
        assert_eq!(
            fake.read_content(&path("src/lib.rs")).as_deref(),
            Some("pub mod utils;\n")
        );

        block_on(fake.write(
            WriteFileRequest {
                path: path("src/new.rs"),
                content: "fn new() {}\n".to_owned(),
            },
            FileToolContext::default(),
        ))
        .expect("write succeeds");
        assert!(fake.read_content(&path("src/new.rs")).is_some());
        block_on(fake.delete(
            DeleteFileRequest {
                path: path("src/new.rs"),
            },
            FileToolContext::default(),
        ))
        .expect("delete succeeds");
        assert!(fake.read_content(&path("src/new.rs")).is_none());

        fake.fail_next(FileToolError::io("disk unavailable"));
        let error = block_on(fake.read(
            ReadFileRequest {
                path: path("src/lib.rs"),
                offset: nonzero(1),
                limit: nonzero(1),
            },
            FileToolContext::default(),
        ))
        .expect_err("injected error");
        assert_eq!(error, FileToolError::io("disk unavailable"));

        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let error = block_on(fake.delete(
            DeleteFileRequest {
                path: path("src/lib.rs"),
            },
            FileToolContext::new(cancellation),
        ))
        .expect_err("cancelled");
        assert_eq!(error, FileToolError::Cancelled);
    }

    #[test]
    fn implicit_directories_and_file_directory_mismatches_are_rejected() {
        let fake = fake();
        assert!(matches!(
            block_on(fake.read(
                ReadFileRequest {
                    path: path("src"),
                    offset: nonzero(1),
                    limit: nonzero(1),
                },
                FileToolContext::default(),
            )),
            Err(FileToolError::UnsupportedFileType { .. })
        ));
        assert!(matches!(
            block_on(fake.list(
                ListDirectoryRequest {
                    path: path("src/main.rs"),
                },
                FileToolContext::default(),
            )),
            Err(FileToolError::UnsupportedFileType { .. })
        ));
        assert!(matches!(
            block_on(fake.write(
                WriteFileRequest {
                    path: path("src"),
                    content: "not a directory anymore".to_owned(),
                },
                FileToolContext::default(),
            )),
            Err(FileToolError::UnsupportedFileType { .. })
        ));
    }
}
