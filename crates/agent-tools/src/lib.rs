//! Agent 工具 SPI、注册表、派发器与内置工具。
//!
//! - [`Tool`] / [`ErasedTool`]：类型化工具抽象与对象安全擦除；serde 反序列化即
//!   输入校验，JSON Schema 由 schemars 从 `Input` 类型派生，两者恒同步。
//! - [`ToolRegistry`] / [`ToolSetSnapshot`]：装配期注册（重名拒绝）、执行期不可变
//!   快照；空快照是合法输入（最小可执行 Agent 不含工具）。
//! - [`Dispatcher`]：单次规范 Tool Call 派发；未知名、校验失败、执行失败都转为
//!   模型可读的错误 `ToolResult`。
//!
//! 文件/Shell 能力契约与内置桥接（`capability`、`builtin` 模块）在 M2 落地。
//! 本 crate 不实现真实文件系统或 Shell 副作用；能力实现与桥接工具注册由
//! Runtime/Adapter 在装配期完成。

mod builtin;
mod capability;
mod dispatch;
mod registry;
mod tool;

pub use builtin::{
    fs::{
        DeleteFileInput, EditFileInput, FindFilesInput, FsDeleteTool, FsEditTool, FsFindTool,
        FsListTool, FsReadTool, FsSearchTool, FsWriteTool, ListDirectoryInput, ReadFileInput,
        SearchContentInput, WriteFileInput,
    },
    shell::{ShellExecTool, ShellInput},
};
pub use capability::{
    fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileEntry,
        FileEntryKind, FileSystemTool, FileToolError, FsFuture, ListDirectoryRequest,
        ListDirectoryResult, ReadFileRequest, ReadFileResult, SearchFilesRequest,
        SearchFilesResult, SearchKind, SearchMatch, WriteFileRequest, WriteFileResult,
        exact_replace, paginate_with_line_numbers,
    },
    shell::{
        ShellFuture, ShellOutcome, ShellOutputChannel, ShellOutputChunk, ShellOutputSink,
        ShellRequest, ShellTool, ShellToolError, tail_truncate,
    },
};
pub use dispatch::Dispatcher;
pub use registry::{RegisterToolError, ToolRegistry, ToolSetSnapshot};
pub use tool::{
    ErasedTool, Tool, ToolContext, ToolError, ToolExecuteFuture, ToolJsonFuture, ToolOutputChannel,
    ToolOutputChunk, ToolOutputSink,
};

#[cfg(test)]
pub(crate) mod testutil {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };

    use agent_types::{ToolCall, ToolCallId, ToolName};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::{Tool, ToolContext, ToolError, ToolExecuteFuture};

    /// 同步驱动一个立即就绪的 Future；测试中的工具实现不允许挂起。
    pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    /// 构造一个规范 Tool Call。
    pub(crate) fn tool_call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new("call_1").expect("valid call id"),
            name: ToolName::new(name).expect("valid tool name"),
            arguments,
        }
    }

    /// 两数相加的最小工具输入。
    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct AddInput {
        pub a: i64,
        pub b: i64,
    }

    /// 两数相加的最小工具输出。
    #[derive(Debug, Serialize)]
    pub(crate) struct AddOutput {
        pub sum: i64,
    }

    /// 两数相加的最小工具。
    pub(crate) struct AddTool;

    impl Tool for AddTool {
        type Input = AddInput;
        type Output = AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("add").expect("valid tool name")
        }

        fn description(&self) -> &str {
            "Add two integers"
        }

        fn execute<'a>(
            &'a self,
            input: AddInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, AddOutput> {
            Box::pin(async move {
                Ok(AddOutput {
                    sum: input.a + input.b,
                })
            })
        }
    }

    /// 总是执行失败的最小工具。
    pub(crate) struct FailTool;

    impl Tool for FailTool {
        type Input = AddInput;
        type Output = AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("fail").expect("valid tool name")
        }

        fn description(&self) -> &str {
            "Always fail"
        }

        fn execute<'a>(
            &'a self,
            _input: AddInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, AddOutput> {
            Box::pin(async move { Err(ToolError::execution("boom")) })
        }
    }

    use std::{collections::BTreeMap, sync::Mutex};

    use crate::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileEntry,
        FileEntryKind, FileSystemTool, FileToolError, FsFuture, ListDirectoryRequest,
        ListDirectoryResult, ReadFileRequest, ReadFileResult, SearchFilesRequest,
        SearchFilesResult, SearchKind, SearchMatch, ShellFuture, ShellOutcome, ShellOutputChunk,
        ShellRequest, ShellTool, ShellToolError, WriteFileRequest, WriteFileResult, exact_replace,
        paginate_with_line_numbers, tail_truncate,
    };
    use tokio_util::sync::CancellationToken;

    /// 内存文件系统最小实现，完整执行六方法契约语义；空目录不可表达。
    pub(crate) struct MiniFileSystem {
        files: Mutex<BTreeMap<String, String>>,
    }

    impl MiniFileSystem {
        pub(crate) fn new<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
            Self {
                files: Mutex::new(
                    files
                        .into_iter()
                        .map(|(path, content)| (path.to_owned(), content.to_owned()))
                        .collect(),
                ),
            }
        }

        pub(crate) fn read_content(&self, path: &str) -> Option<String> {
            self.files.lock().expect("lock files").get(path).cloned()
        }

        fn dir_exists(files: &BTreeMap<String, String>, path: &str) -> bool {
            path.is_empty() || files.keys().any(|key| key.starts_with(&format!("{path}/")))
        }

        fn prefixed(files: &BTreeMap<String, String>, path: &Option<String>) -> Vec<String> {
            files
                .keys()
                .filter(|key| match path {
                    None => true,
                    Some(path) if path.is_empty() => true,
                    Some(path) => key.starts_with(&format!("{path}/")),
                })
                .cloned()
                .collect()
        }
    }

    impl FileSystemTool for MiniFileSystem {
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
                for path in Self::prefixed(&files, &request.path) {
                    let content = files.get(&path).expect("path from keys");
                    match request.kind {
                        SearchKind::ByName => {
                            let name = path.rsplit('/').next().unwrap_or(&path);
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

    /// 脚本化 Shell 最小实现：回放固定 chunk 流，按请求上限聚合截断，观察取消。
    pub(crate) struct MiniShell {
        pub(crate) chunks: Vec<ShellOutputChunk>,
        pub(crate) exit_code: i32,
    }

    impl ShellTool for MiniShell {
        fn exec<'a>(
            &'a self,
            request: ShellRequest,
            sink: crate::ShellOutputSink,
            cancellation: CancellationToken,
        ) -> ShellFuture<'a> {
            Box::pin(async move {
                if request.command.is_empty() {
                    return Err(ShellToolError::InvalidInput {
                        message: "command must not be empty".to_owned(),
                    });
                }
                let mut aggregated = String::new();
                for chunk in &self.chunks {
                    if cancellation.is_cancelled() {
                        return Err(ShellToolError::Cancelled);
                    }
                    sink(chunk.clone());
                    aggregated.push_str(&chunk.data);
                }
                let (aggregated, truncated) = tail_truncate(&aggregated, request.max_output_bytes);
                Ok(ShellOutcome {
                    exit_code: Some(self.exit_code),
                    timed_out: false,
                    aggregated,
                    truncated,
                })
            })
        }
    }
}
