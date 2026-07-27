//! 文件能力桥接工具：把 [`FileSystemTool`] 包装为模型可见的类型化 [`Tool`]。
//!
//! 桥接零格式化（行号、截断等语义由能力层契约保证）；默认截断上限为构造参数，
//! 由 Runtime 装配时给定。描述文本承担行为纪律，随快照原样下发。

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    capability::fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileSystemTool,
        FileToolError, ListDirectoryRequest, ListDirectoryResult, ReadFileRequest, ReadFileResult,
        SearchFilesRequest, SearchFilesResult, SearchKind, WriteFileRequest, WriteFileResult,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture},
};
use agent_types::ToolName;

/// 能力错误到工具错误的映射：参数违反契约 → `InvalidInput`，其余 → `Execution`。
fn map_fs_error(error: FileToolError) -> ToolError {
    match error {
        FileToolError::InvalidInput { message } => ToolError::invalid_input(message),
        FileToolError::NotFound { .. } | FileToolError::Io { .. } => {
            ToolError::execution(error.to_string())
        }
    }
}

/// `read_file`：读取文件，内容带 1 起始行号。
pub struct FsReadTool {
    fs: Arc<dyn FileSystemTool>,
    default_limit: u32,
}

/// `read_file` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileInput {
    /// 文件路径。
    pub path: String,
    /// 起始行号（1 起始，含）；缺省从第 1 行开始。
    pub offset: Option<u32>,
    /// 最多返回行数；缺省使用装配默认值。
    pub limit: Option<u32>,
}

impl FsReadTool {
    /// 创建读取桥接工具；`default_limit` 为模型未给 `limit` 时的默认行数。
    pub fn new(fs: Arc<dyn FileSystemTool>, default_limit: u32) -> Self {
        Self { fs, default_limit }
    }
}

impl Tool for FsReadTool {
    type Input = ReadFileInput;
    type Output = ReadFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("read_file").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Read a file. Content is returned with 1-based line numbers (`{line}\\t{text}` per \
         line). Use offset/limit to page through large files instead of reading them whole. \
         Do NOT use the shell tool to read files."
    }

    fn execute<'a>(
        &'a self,
        input: ReadFileInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, ReadFileResult> {
        Box::pin(async move {
            self.fs
                .read(ReadFileRequest {
                    path: input.path,
                    offset: input.offset,
                    limit: Some(input.limit.unwrap_or(self.default_limit)),
                })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `list_directory`：非递归列目录。
pub struct FsListTool {
    fs: Arc<dyn FileSystemTool>,
}

/// `list_directory` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryInput {
    /// 目录路径。
    pub path: String,
}

impl FsListTool {
    /// 创建列目录桥接工具。
    pub fn new(fs: Arc<dyn FileSystemTool>) -> Self {
        Self { fs }
    }
}

impl Tool for FsListTool {
    type Input = ListDirectoryInput;
    type Output = ListDirectoryResult;

    fn name(&self) -> ToolName {
        ToolName::new("list_directory").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "List the direct children of a directory (non-recursive) with entry kinds. Do NOT use \
         the shell tool to list directories."
    }

    fn execute<'a>(
        &'a self,
        input: ListDirectoryInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, ListDirectoryResult> {
        Box::pin(async move {
            self.fs
                .list(ListDirectoryRequest { path: input.path })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `find_files`：按文件名子串查找文件。
pub struct FsFindTool {
    fs: Arc<dyn FileSystemTool>,
    default_max_results: u32,
}

/// `find_files` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindFilesInput {
    /// 文件名包含的字面量子串。
    pub query: String,
    /// 搜索起始路径；缺省为工作区根。
    pub path: Option<String>,
    /// 最多返回匹配数；缺省使用装配默认值。
    pub max_results: Option<u32>,
}

impl FsFindTool {
    /// 创建按名查找桥接工具；`default_max_results` 为默认匹配上限。
    pub fn new(fs: Arc<dyn FileSystemTool>, default_max_results: u32) -> Self {
        Self {
            fs,
            default_max_results,
        }
    }
}

impl Tool for FsFindTool {
    type Input = FindFilesInput;
    type Output = SearchFilesResult;

    fn name(&self) -> ToolName {
        ToolName::new("find_files").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Find files whose file name contains the query substring. Narrow the query or path \
         when results are truncated. Do NOT use the shell tool to find files."
    }

    fn execute<'a>(
        &'a self,
        input: FindFilesInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            self.fs
                .search(SearchFilesRequest {
                    query: input.query,
                    path: input.path,
                    kind: SearchKind::ByName,
                    max_results: Some(input.max_results.unwrap_or(self.default_max_results)),
                })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `search_content`：按内容子串搜索文件。
pub struct FsSearchTool {
    fs: Arc<dyn FileSystemTool>,
    default_max_results: u32,
}

/// `search_content` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchContentInput {
    /// 内容包含的字面量子串。
    pub query: String,
    /// 搜索起始路径；缺省为工作区根。
    pub path: Option<String>,
    /// 最多返回匹配数；缺省使用装配默认值。
    pub max_results: Option<u32>,
}

impl FsSearchTool {
    /// 创建内容搜索桥接工具；`default_max_results` 为默认匹配上限。
    pub fn new(fs: Arc<dyn FileSystemTool>, default_max_results: u32) -> Self {
        Self {
            fs,
            default_max_results,
        }
    }
}

impl Tool for FsSearchTool {
    type Input = SearchContentInput;
    type Output = SearchFilesResult;

    fn name(&self) -> ToolName {
        ToolName::new("search_content").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Search file contents for a literal substring; returns matching lines with 1-based \
         line numbers. Do NOT use the shell tool (e.g. grep/rg) to search content."
    }

    fn execute<'a>(
        &'a self,
        input: SearchContentInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, SearchFilesResult> {
        Box::pin(async move {
            self.fs
                .search(SearchFilesRequest {
                    query: input.query,
                    path: input.path,
                    kind: SearchKind::ByContent,
                    max_results: Some(input.max_results.unwrap_or(self.default_max_results)),
                })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `write_file`：覆盖写入或新建文件。
pub struct FsWriteTool {
    fs: Arc<dyn FileSystemTool>,
}

/// `write_file` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileInput {
    /// 文件路径。
    pub path: String,
    /// 完整新内容。
    pub content: String,
}

impl FsWriteTool {
    /// 创建写入桥接工具。
    pub fn new(fs: Arc<dyn FileSystemTool>) -> Self {
        Self { fs }
    }
}

impl Tool for FsWriteTool {
    type Input = WriteFileInput;
    type Output = WriteFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("write_file").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content. For partial modifications of \
         existing files, prefer edit_file."
    }

    fn execute<'a>(
        &'a self,
        input: WriteFileInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, WriteFileResult> {
        Box::pin(async move {
            self.fs
                .write(WriteFileRequest {
                    path: input.path,
                    content: input.content,
                })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `edit_file`：精确字符串替换编辑。
pub struct FsEditTool {
    fs: Arc<dyn FileSystemTool>,
}

/// `edit_file` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileInput {
    /// 文件路径。
    pub path: String,
    /// 被替换的原文（必须与文件内容完全一致）。
    pub old_string: String,
    /// 替换后的新内容。
    pub new_string: String,
    /// 替换全部匹配；缺省要求唯一匹配。
    pub replace_all: Option<bool>,
}

impl FsEditTool {
    /// 创建编辑桥接工具。
    pub fn new(fs: Arc<dyn FileSystemTool>) -> Self {
        Self { fs }
    }
}

impl Tool for FsEditTool {
    type Input = EditFileInput;
    type Output = EditFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("edit_file").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. Fails when the string is missing or occurs \
         multiple times unless replace_all is set. Prefer this over write_file for partial \
         modifications."
    }

    fn execute<'a>(
        &'a self,
        input: EditFileInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, EditFileResult> {
        Box::pin(async move {
            self.fs
                .edit(EditFileRequest {
                    path: input.path,
                    old_string: input.old_string,
                    new_string: input.new_string,
                    replace_all: input.replace_all.unwrap_or(false),
                })
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `delete_file`：删除文件或空目录。
pub struct FsDeleteTool {
    fs: Arc<dyn FileSystemTool>,
}

/// `delete_file` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteFileInput {
    /// 文件或空目录路径。
    pub path: String,
}

impl FsDeleteTool {
    /// 创建删除桥接工具。
    pub fn new(fs: Arc<dyn FileSystemTool>) -> Self {
        Self { fs }
    }
}

impl Tool for FsDeleteTool {
    type Input = DeleteFileInput;
    type Output = DeleteFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("delete_file").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Delete a file or an empty directory."
    }

    fn execute<'a>(
        &'a self,
        input: DeleteFileInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, DeleteFileResult> {
        Box::pin(async move {
            self.fs
                .delete(DeleteFileRequest { path: input.path })
                .await
                .map_err(map_fs_error)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        FileEntryKind, SearchMatch,
        testutil::{MiniFileSystem, block_on},
    };

    fn mini_fs() -> Arc<MiniFileSystem> {
        Arc::new(MiniFileSystem::new([
            (
                "src/main.rs",
                "fn main() {}\nfn helper() {}\nfn tail() {}\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod helper;\n"),
            ("README.md", "# demo\n"),
        ]))
    }

    #[test]
    fn read_pages_with_line_numbers_and_truncation_marker() {
        let tool = FsReadTool::new(mini_fs(), 2);

        // 默认行数生效并带截断标记。
        let result = block_on(tool.execute(
            ReadFileInput {
                path: "src/main.rs".to_owned(),
                offset: None,
                limit: None,
            },
            ToolContext::default(),
        ))
        .expect("read succeeds");
        assert_eq!(result.content, "1\tfn main() {}\n2\tfn helper() {}");
        assert!(result.truncated);

        // 显式分页：offset 生效。
        let result = block_on(tool.execute(
            ReadFileInput {
                path: "src/main.rs".to_owned(),
                offset: Some(2),
                limit: None,
            },
            ToolContext::default(),
        ))
        .expect("read succeeds");
        assert_eq!(result.content, "2\tfn helper() {}\n3\tfn tail() {}");
        assert!(!result.truncated);

        // 不存在的文件 → 执行类错误。
        let error = block_on(tool.execute(
            ReadFileInput {
                path: "missing.rs".to_owned(),
                offset: None,
                limit: None,
            },
            ToolContext::default(),
        ))
        .expect_err("missing file must fail");
        assert!(matches!(error, ToolError::Execution { .. }));
    }

    #[test]
    fn list_returns_sorted_entries_with_kinds() {
        let tool = FsListTool::new(mini_fs());
        let result = block_on(tool.execute(
            ListDirectoryInput {
                path: String::new(),
            },
            ToolContext::default(),
        ))
        .expect("list succeeds");
        let entries: Vec<(String, FileEntryKind)> = result
            .entries
            .into_iter()
            .map(|entry| (entry.name, entry.kind))
            .collect();
        assert_eq!(
            entries,
            [
                ("README.md".to_owned(), FileEntryKind::File),
                ("src".to_owned(), FileEntryKind::Directory),
            ]
        );
    }

    #[test]
    fn find_files_matches_by_name_with_result_cap() {
        let tool = FsFindTool::new(mini_fs(), 10);
        let result = block_on(tool.execute(
            FindFilesInput {
                query: "lib".to_owned(),
                path: None,
                max_results: None,
            },
            ToolContext::default(),
        ))
        .expect("find succeeds");
        assert_eq!(
            result.matches,
            [SearchMatch::Name {
                path: "src/lib.rs".to_owned()
            }]
        );
        assert!(!result.truncated);

        // 上限截断。
        let result = block_on(tool.execute(
            FindFilesInput {
                query: "rs".to_owned(),
                path: None,
                max_results: Some(1),
            },
            ToolContext::default(),
        ))
        .expect("find succeeds");
        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
    }

    #[test]
    fn search_content_matches_lines_with_line_numbers() {
        let tool = FsSearchTool::new(mini_fs(), 10);
        let result = block_on(tool.execute(
            SearchContentInput {
                query: "helper".to_owned(),
                path: None,
                max_results: None,
            },
            ToolContext::default(),
        ))
        .expect("search succeeds");
        assert_eq!(
            result.matches,
            [
                SearchMatch::Content {
                    path: "src/lib.rs".to_owned(),
                    line_number: 2,
                    line: "pub mod helper;".to_owned(),
                },
                SearchMatch::Content {
                    path: "src/main.rs".to_owned(),
                    line_number: 2,
                    line: "fn helper() {}".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn write_creates_and_overwrites() {
        let fs = mini_fs();
        let tool = FsWriteTool::new(fs.clone());

        let result = block_on(tool.execute(
            WriteFileInput {
                path: "notes.md".to_owned(),
                content: "hello".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect("write succeeds");
        assert_eq!(result.bytes_written, 5);
        assert_eq!(fs.read_content("notes.md").as_deref(), Some("hello"));

        let result = block_on(tool.execute(
            WriteFileInput {
                path: "notes.md".to_owned(),
                content: "updated".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect("overwrite succeeds");
        assert_eq!(result.bytes_written, 7);
        assert_eq!(fs.read_content("notes.md").as_deref(), Some("updated"));

        // 父目录不存在 → 执行类错误。
        let error = block_on(tool.execute(
            WriteFileInput {
                path: "no/such/dir/file.md".to_owned(),
                content: "x".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect_err("missing parent must fail");
        assert!(matches!(error, ToolError::Execution { .. }));
    }

    #[test]
    fn edit_enforces_exact_match_semantics() {
        let fs = mini_fs();
        let tool = FsEditTool::new(fs.clone());

        // 唯一匹配成功。
        let result = block_on(tool.execute(
            EditFileInput {
                path: "src/lib.rs".to_owned(),
                old_string: "pub mod a;".to_owned(),
                new_string: "pub mod c;".to_owned(),
                replace_all: None,
            },
            ToolContext::default(),
        ))
        .expect("edit succeeds");
        assert_eq!(result.replacements, 1);
        assert!(
            fs.read_content("src/lib.rs")
                .expect("file")
                .contains("pub mod c;")
        );

        // old == new → 参数错误。
        let error = block_on(tool.execute(
            EditFileInput {
                path: "src/lib.rs".to_owned(),
                old_string: "x".to_owned(),
                new_string: "x".to_owned(),
                replace_all: None,
            },
            ToolContext::default(),
        ))
        .expect_err("old == new must fail");
        assert!(matches!(error, ToolError::InvalidInput { .. }));

        // 多处匹配且未设 replace_all → 参数错误。
        let error = block_on(tool.execute(
            EditFileInput {
                path: "src/main.rs".to_owned(),
                old_string: "fn ".to_owned(),
                new_string: "fn x_".to_owned(),
                replace_all: None,
            },
            ToolContext::default(),
        ))
        .expect_err("multiple matches must fail");
        assert!(matches!(error, ToolError::InvalidInput { .. }));

        // replace_all 替换全部。
        let result = block_on(tool.execute(
            EditFileInput {
                path: "src/main.rs".to_owned(),
                old_string: "fn ".to_owned(),
                new_string: "fn x_".to_owned(),
                replace_all: Some(true),
            },
            ToolContext::default(),
        ))
        .expect("replace_all succeeds");
        assert_eq!(result.replacements, 3);

        // 文件不存在 → 执行类错误。
        let error = block_on(tool.execute(
            EditFileInput {
                path: "missing.rs".to_owned(),
                old_string: "a".to_owned(),
                new_string: "b".to_owned(),
                replace_all: None,
            },
            ToolContext::default(),
        ))
        .expect_err("missing file must fail");
        assert!(matches!(error, ToolError::Execution { .. }));
    }

    #[test]
    fn delete_removes_files_and_rejects_missing_paths() {
        let fs = mini_fs();
        let tool = FsDeleteTool::new(fs.clone());

        let result = block_on(tool.execute(
            DeleteFileInput {
                path: "README.md".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect("delete succeeds");
        assert_eq!(result.deleted, "README.md");
        assert!(fs.read_content("README.md").is_none());

        let error = block_on(tool.execute(
            DeleteFileInput {
                path: "README.md".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect_err("deleting twice must fail");
        assert!(matches!(error, ToolError::Execution { .. }));

        // 非空目录 → 参数错误。
        let error = block_on(tool.execute(
            DeleteFileInput {
                path: "src".to_owned(),
            },
            ToolContext::default(),
        ))
        .expect_err("non-empty directory must fail");
        assert!(matches!(error, ToolError::InvalidInput { .. }));
    }
}
