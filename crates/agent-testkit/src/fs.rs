//! 内存文件树 Fake：完整执行 [`FileSystemTool`] 六方法契约语义。
//!
//! 目录由文件路径隐式表达，空目录不可表达；分页行号、精确替换、截断标记等
//! 契约语义复用 `agent-tools` 的契约级辅助函数，与真实实现同构。

use std::{collections::BTreeMap, sync::Mutex};

use agent_tools::{
    DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileEntry, FileEntryKind,
    FileSystemTool, FileToolError, FsFuture, ListDirectoryRequest, ListDirectoryResult,
    ReadFileRequest, ReadFileResult, SearchFilesRequest, SearchFilesResult, SearchKind,
    SearchMatch, WriteFileRequest, WriteFileResult, exact_replace, paginate_with_line_numbers,
};

/// 内存文件树 Fake；路径为 `/` 分隔的相对路径。
pub struct FakeFileSystemTool {
    files: Mutex<BTreeMap<String, String>>,
}

impl FakeFileSystemTool {
    /// 用初始文件集创建 Fake。
    pub fn new<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            files: Mutex::new(
                files
                    .into_iter()
                    .map(|(path, content)| (path.to_owned(), content.to_owned()))
                    .collect(),
            ),
        }
    }

    /// 读取当前文件内容（断言用）。
    pub fn read_content(&self, path: &str) -> Option<String> {
        self.files.lock().expect("lock files").get(path).cloned()
    }

    /// 当前文件树快照（断言用）。
    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.files.lock().expect("lock files").clone()
    }

    fn dir_exists(files: &BTreeMap<String, String>, path: &str) -> bool {
        path.is_empty() || files.keys().any(|key| key.starts_with(&format!("{path}/")))
    }
}

impl FileSystemTool for FakeFileSystemTool {
    fn read<'a>(&'a self, request: ReadFileRequest) -> FsFuture<'a, ReadFileResult> {
        Box::pin(async move {
            let files = self.files.lock().expect("lock files");
            let Some(content) = files.get(&request.path) else {
                return Err(FileToolError::NotFound { path: request.path });
            };
            let (content, truncated) =
                paginate_with_line_numbers(content, request.offset, request.limit);
            Ok(ReadFileResult { content, truncated })
        })
    }

    fn list<'a>(&'a self, request: ListDirectoryRequest) -> FsFuture<'a, ListDirectoryResult> {
        Box::pin(async move {
            let files = self.files.lock().expect("lock files");
            if !Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::NotFound { path: request.path });
            }
            let prefix = if request.path.is_empty() {
                String::new()
            } else {
                format!("{}/", request.path)
            };
            let mut entries: Vec<FileEntry> = Vec::new();
            for key in files.keys() {
                let Some(rest) = key.strip_prefix(&prefix) else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                let (name, kind) = match rest.split_once('/') {
                    Some((directory, _)) => (directory, FileEntryKind::Directory),
                    None => (rest, FileEntryKind::File),
                };
                let entry = FileEntry {
                    name: name.to_owned(),
                    kind,
                };
                if !entries.contains(&entry) {
                    entries.push(entry);
                }
            }
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            Ok(ListDirectoryResult { entries })
        })
    }

    fn search<'a>(&'a self, request: SearchFilesRequest) -> FsFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            if request.query.is_empty() {
                return Err(FileToolError::invalid_input("query must not be empty"));
            }
            let files = self.files.lock().expect("lock files");
            let mut matches = Vec::new();
            for (path, content) in files.iter() {
                let in_scope = match &request.path {
                    None => true,
                    Some(path_prefix) if path_prefix.is_empty() => true,
                    Some(path_prefix) => path.starts_with(&format!("{path_prefix}/")),
                };
                if !in_scope {
                    continue;
                }
                match request.kind {
                    SearchKind::ByName => {
                        let name = path.rsplit('/').next().unwrap_or(path);
                        if name.contains(&request.query) {
                            matches.push(SearchMatch::Name { path: path.clone() });
                        }
                    }
                    SearchKind::ByContent => {
                        for (index, line) in content.lines().enumerate() {
                            if line.contains(&request.query) {
                                matches.push(SearchMatch::Content {
                                    path: path.clone(),
                                    line_number: index as u32 + 1,
                                    line: line.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            let limit = request.max_results.map(|limit| limit as usize);
            let truncated = limit.is_some_and(|limit| matches.len() > limit);
            if let Some(limit) = limit {
                matches.truncate(limit);
            }
            Ok(SearchFilesResult { matches, truncated })
        })
    }

    fn write<'a>(&'a self, request: WriteFileRequest) -> FsFuture<'a, WriteFileResult> {
        Box::pin(async move {
            let mut files = self.files.lock().expect("lock files");
            let parent = request
                .path
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("");
            if !Self::dir_exists(&files, parent) {
                return Err(FileToolError::NotFound {
                    path: parent.to_owned(),
                });
            }
            let bytes_written = request.content.len() as u64;
            files.insert(request.path, request.content);
            Ok(WriteFileResult { bytes_written })
        })
    }

    fn delete<'a>(&'a self, request: DeleteFileRequest) -> FsFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            let mut files = self.files.lock().expect("lock files");
            if files.remove(&request.path).is_some() {
                return Ok(DeleteFileResult {
                    deleted: request.path,
                });
            }
            if Self::dir_exists(&files, &request.path) {
                return Err(FileToolError::invalid_input(
                    "directory is not empty".to_owned(),
                ));
            }
            Err(FileToolError::NotFound { path: request.path })
        })
    }

    fn edit<'a>(&'a self, request: EditFileRequest) -> FsFuture<'a, EditFileResult> {
        Box::pin(async move {
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
            files.insert(request.path, replaced);
            Ok(EditFileResult { replacements })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
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

    fn fake() -> FakeFileSystemTool {
        FakeFileSystemTool::new([
            ("src/main.rs", "fn main() {}\nfn helper() {}\n"),
            ("src/lib.rs", "pub mod helper;\n"),
        ])
    }

    #[test]
    fn read_paginates_with_line_numbers() {
        let result = block_on(fake().read(ReadFileRequest {
            path: "src/main.rs".to_owned(),
            offset: Some(2),
            limit: None,
        }))
        .expect("read succeeds");
        assert_eq!(result.content, "2\tfn helper() {}");
        assert!(!result.truncated);
    }

    #[test]
    fn search_supports_both_kinds_with_scope() {
        let by_name = block_on(fake().search(SearchFilesRequest {
            query: "main".to_owned(),
            path: None,
            kind: SearchKind::ByName,
            max_results: None,
        }))
        .expect("search succeeds");
        assert_eq!(
            by_name.matches,
            [SearchMatch::Name {
                path: "src/main.rs".to_owned()
            }]
        );

        let by_content = block_on(fake().search(SearchFilesRequest {
            query: "helper".to_owned(),
            path: Some("src".to_owned()),
            kind: SearchKind::ByContent,
            max_results: None,
        }))
        .expect("search succeeds");
        assert_eq!(by_content.matches.len(), 2);
        assert!(!by_content.truncated);
    }

    #[test]
    fn mutations_are_visible_in_later_reads() {
        let fake = fake();
        block_on(fake.edit(EditFileRequest {
            path: "src/lib.rs".to_owned(),
            old_string: "helper".to_owned(),
            new_string: "utils".to_owned(),
            replace_all: false,
        }))
        .expect("edit succeeds");
        assert_eq!(
            fake.read_content("src/lib.rs").as_deref(),
            Some("pub mod utils;\n")
        );

        block_on(fake.write(WriteFileRequest {
            path: "src/new.rs".to_owned(),
            content: "fn new() {}\n".to_owned(),
        }))
        .expect("write succeeds");
        assert!(fake.read_content("src/new.rs").is_some());

        block_on(fake.delete(DeleteFileRequest {
            path: "src/new.rs".to_owned(),
        }))
        .expect("delete succeeds");
        assert!(fake.read_content("src/new.rs").is_none());
    }
}
