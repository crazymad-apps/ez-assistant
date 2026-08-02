//! 标准文件工具壳：把模型输入解析为绝对逻辑路径、有效默认值与类型化授权事实。
//!
//! resolve 阶段只做纯计算；文件是否存在、符号链接和真实读写全部委托给注入的
//! [`FileSystemTool`]。

use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult,
        FileAuthorizationFacts, FileOperation, FileSystemTool, FileToolContext, FileToolError,
        ListDirectoryRequest, ListDirectoryResult, ReadFileRequest, ReadFileResult,
        SearchFilesRequest, SearchFilesResult, SearchKind, WriteFileRequest, WriteFileResult,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults},
};
use agent_types::ToolName;

use super::ToolConfigurationError;

/// read_file 的默认分页与单次上限配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadFileToolConfig {
    default_offset: NonZeroU32,
    default_limit: NonZeroU32,
    maximum_limit: NonZeroU32,
}

impl ReadFileToolConfig {
    /// 创建读取配置；默认单页行数不得超过实例允许的最大行数。
    pub fn new(
        default_offset: NonZeroU32,
        default_limit: NonZeroU32,
        maximum_limit: NonZeroU32,
    ) -> Result<Self, ToolConfigurationError> {
        if default_limit > maximum_limit {
            return Err(ToolConfigurationError::new(
                "read default_limit must not exceed maximum_limit",
            ));
        }
        Ok(Self {
            default_offset,
            default_limit,
            maximum_limit,
        })
    }

    /// 未传 `offset` 时采用的 1 起始行号。
    pub fn default_offset(&self) -> NonZeroU32 {
        self.default_offset
    }

    /// 未传 `limit` 时采用的单页行数。
    pub fn default_limit(&self) -> NonZeroU32 {
        self.default_limit
    }

    /// 单次模型调用允许请求的最大行数。
    pub fn maximum_limit(&self) -> NonZeroU32 {
        self.maximum_limit
    }
}

/// find_files/search_content 的默认结果数与单次上限配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchFilesToolConfig {
    default_max_results: NonZeroU32,
    maximum_max_results: NonZeroU32,
    max_output_bytes: NonZeroU64,
    max_record_bytes: NonZeroU64,
}

impl SearchFilesToolConfig {
    /// 创建搜索配置；默认结果数不得超过实例允许的最大结果数。
    pub fn new(
        default_max_results: NonZeroU32,
        maximum_max_results: NonZeroU32,
        max_output_bytes: NonZeroU64,
        max_record_bytes: NonZeroU64,
    ) -> Result<Self, ToolConfigurationError> {
        if default_max_results > maximum_max_results {
            return Err(ToolConfigurationError::new(
                "search default_max_results must not exceed maximum_max_results",
            ));
        }
        Ok(Self {
            default_max_results,
            maximum_max_results,
            max_output_bytes,
            max_record_bytes,
        })
    }

    /// 未传 `max_results` 时采用的结果数。
    pub fn default_max_results(&self) -> NonZeroU32 {
        self.default_max_results
    }

    /// 单次模型调用允许请求的最大结果数。
    pub fn maximum_max_results(&self) -> NonZeroU32 {
        self.maximum_max_results
    }

    /// 单次搜索允许读取的 stdout 总字节数。
    pub fn max_output_bytes(&self) -> NonZeroU64 {
        self.max_output_bytes
    }

    /// 单条 NUL/JSON 记录允许读取的最大字节数。
    pub fn max_record_bytes(&self) -> NonZeroU64 {
        self.max_record_bytes
    }
}

fn map_fs_error(error: FileToolError) -> ToolError {
    match error {
        FileToolError::InvalidInput { message } => ToolError::invalid_input(message),
        // Engine 在执行取消后会直接收敛到 Cancelled，不会把映射后的文本回喂模型；
        // Cancelled 落入此分支只为能力错误到通用 ToolError 的类型映射保持完备。
        other => ToolError::execution(other.to_string()),
    }
}

fn resolve_path(
    resolver: &SessionPathResolver,
    input: &str,
) -> Result<crate::AbsolutePath, ToolError> {
    resolver
        .resolve(input)
        .map_err(|error| ToolError::invalid_input(error.to_string()))
}

fn file_resolution<T: Serialize>(
    input: T,
    operation: FileOperation,
    path: crate::AbsolutePath,
) -> Result<ToolResolution<T>, ToolError> {
    let mut semantic_arguments = serde_json::to_value(&input).map_err(|error| {
        ToolError::invalid_input(format!("cannot serialize resolved input: {error}"))
    })?;
    let Value::Object(arguments) = &mut semantic_arguments else {
        return Err(ToolError::invalid_input(
            "resolved file input must serialize as an object",
        ));
    };
    arguments.insert(
        "operation".to_owned(),
        serde_json::to_value(operation)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?,
    );
    Ok(ToolResolution::with_facts(
        input,
        FileAuthorizationFacts { operation, path },
        semantic_arguments,
    ))
}

fn file_context(context: ToolContext) -> FileToolContext {
    FileToolContext::new(context.cancellation)
}

/// `read_file` 标准工具壳。
pub struct FsReadTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
    config: ReadFileToolConfig,
}

/// `read_file` 的模型输入；分页参数省略时使用 Schema 中公开的实例默认值。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileInput {
    /// 绝对路径或相对 Session 工作目录的文件路径。
    pub path: String,
    /// 1 起始的首行行号。
    pub offset: Option<NonZeroU32>,
    /// 最多返回的行数。
    pub limit: Option<NonZeroU32>,
}

impl FsReadTool {
    /// 用文件能力、Session 路径解析器和分页配置装配工具壳。
    pub fn new(
        fs: Arc<dyn FileSystemTool>,
        resolver: SessionPathResolver,
        config: ReadFileToolConfig,
    ) -> Self {
        Self {
            fs,
            resolver,
            config,
        }
    }
}

impl Tool for FsReadTool {
    type Input = ReadFileInput;
    type ResolvedInput = ReadFileRequest;
    type Output = ReadFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("read_file").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Read a UTF-8 text file with 1-based line numbers. Default offset: {}; default \
             limit: {}; maximum limit: {}. Use offset/limit for paging.",
            self.config.default_offset, self.config.default_limit, self.config.maximum_limit,
        )
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("offset", self.config.default_offset)
            .with("limit", self.config.default_limit)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        let limit = input.limit.unwrap_or(self.config.default_limit);
        if limit > self.config.maximum_limit {
            return Err(ToolError::invalid_input(format!(
                "limit must not exceed {}",
                self.config.maximum_limit
            )));
        }
        file_resolution(
            ReadFileRequest {
                path: path.clone(),
                offset: input.offset.unwrap_or(self.config.default_offset),
                limit,
            },
            FileOperation::Read,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .read(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `list_directory` 标准工具壳。
pub struct FsListTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
}

/// `list_directory` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryInput {
    /// 绝对路径或相对 Session 工作目录的目录路径。
    pub path: String,
}

impl FsListTool {
    /// 用文件能力和 Session 路径解析器装配工具壳。
    pub fn new(fs: Arc<dyn FileSystemTool>, resolver: SessionPathResolver) -> Self {
        Self { fs, resolver }
    }
}

impl Tool for FsListTool {
    type Input = ListDirectoryInput;
    type ResolvedInput = ListDirectoryRequest;
    type Output = ListDirectoryResult;

    fn name(&self) -> ToolName {
        ToolName::new("list_directory").expect("valid tool name")
    }

    fn description(&self) -> String {
        "List direct children with absolute paths, target kinds and a separate symlink flag."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        file_resolution(
            ListDirectoryRequest { path: path.clone() },
            FileOperation::List,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .list(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `find_files` 标准工具壳；按文件名字面量子串查找。
pub struct FsFindTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
    config: SearchFilesToolConfig,
}

/// `find_files` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindFilesInput {
    /// 文件名中需要包含的非空字面量子串。
    pub query: String,
    /// 缺省使用冻结的 Session 工作目录。
    pub path: Option<String>,
    /// 最多返回的匹配数。
    pub max_results: Option<NonZeroU32>,
}

impl FsFindTool {
    /// 用文件能力、Session 路径解析器和结果数配置装配工具壳。
    pub fn new(
        fs: Arc<dyn FileSystemTool>,
        resolver: SessionPathResolver,
        config: SearchFilesToolConfig,
    ) -> Self {
        Self {
            fs,
            resolver,
            config,
        }
    }
}

impl Tool for FsFindTool {
    type Input = FindFilesInput;
    type ResolvedInput = SearchFilesRequest;
    type Output = SearchFilesResult;

    fn name(&self) -> ToolName {
        ToolName::new("find_files").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Find files by literal name substring. Default path: {}; default result limit: {}; \
             maximum: {}; search output limit: {} bytes; per-record limit: {} bytes.",
            self.resolver.session_workdir(),
            self.config.default_max_results,
            self.config.maximum_max_results,
            self.config.max_output_bytes,
            self.config.max_record_bytes,
        )
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("path", self.resolver.session_workdir().as_str())
            .with("max_results", self.config.default_max_results)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        resolve_search(
            &self.resolver,
            self.config,
            input.query,
            input.path,
            input.max_results,
            SearchKind::ByName,
            FileOperation::Find,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .search(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `search_content` 标准工具壳；按文本内容字面量子串搜索。
pub struct FsSearchTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
    config: SearchFilesToolConfig,
}

/// `search_content` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchContentInput {
    /// 文件内容中需要包含的非空字面量子串。
    pub query: String,
    /// 搜索根路径；省略时使用 Session 工作目录。
    pub path: Option<String>,
    /// 最多返回的匹配数。
    pub max_results: Option<NonZeroU32>,
}

impl FsSearchTool {
    /// 用文件能力、Session 路径解析器和结果数配置装配工具壳。
    pub fn new(
        fs: Arc<dyn FileSystemTool>,
        resolver: SessionPathResolver,
        config: SearchFilesToolConfig,
    ) -> Self {
        Self {
            fs,
            resolver,
            config,
        }
    }
}

impl Tool for FsSearchTool {
    type Input = SearchContentInput;
    type ResolvedInput = SearchFilesRequest;
    type Output = SearchFilesResult;

    fn name(&self) -> ToolName {
        ToolName::new("search_content").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Search UTF-8 file contents for a literal substring. Default path: {}; default \
             result limit: {}; maximum: {}; search output limit: {} bytes; per-record limit: {} \
             bytes.",
            self.resolver.session_workdir(),
            self.config.default_max_results,
            self.config.maximum_max_results,
            self.config.max_output_bytes,
            self.config.max_record_bytes,
        )
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("path", self.resolver.session_workdir().as_str())
            .with("max_results", self.config.default_max_results)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        resolve_search(
            &self.resolver,
            self.config,
            input.query,
            input.path,
            input.max_results,
            SearchKind::ByContent,
            FileOperation::Search,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .search(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

fn resolve_search(
    resolver: &SessionPathResolver,
    config: SearchFilesToolConfig,
    query: String,
    path: Option<String>,
    max_results: Option<NonZeroU32>,
    kind: SearchKind,
    operation: FileOperation,
) -> Result<ToolResolution<SearchFilesRequest>, ToolError> {
    if query.is_empty() {
        return Err(ToolError::invalid_input("query must not be empty"));
    }
    let path = match path {
        Some(path) => resolve_path(resolver, &path)?,
        None => resolver.session_workdir().clone(),
    };
    let max_results = max_results.unwrap_or(config.default_max_results);
    if max_results > config.maximum_max_results {
        return Err(ToolError::invalid_input(format!(
            "max_results must not exceed {}",
            config.maximum_max_results
        )));
    }
    file_resolution(
        SearchFilesRequest {
            query,
            path: path.clone(),
            kind,
            max_results,
            max_output_bytes: config.max_output_bytes,
            max_record_bytes: config.max_record_bytes,
        },
        operation,
        path,
    )
}

/// `write_file` 标准工具壳。
pub struct FsWriteTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
}

/// `write_file` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileInput {
    /// 绝对路径或相对 Session 工作目录的目标路径。
    pub path: String,
    /// 需要完整写入的 UTF-8 文本。
    pub content: String,
}

impl FsWriteTool {
    /// 用文件能力和 Session 路径解析器装配工具壳。
    pub fn new(fs: Arc<dyn FileSystemTool>, resolver: SessionPathResolver) -> Self {
        Self { fs, resolver }
    }
}

impl Tool for FsWriteTool {
    type Input = WriteFileInput;
    type ResolvedInput = WriteFileRequest;
    type Output = WriteFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("write_file").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Create or overwrite a UTF-8 text file; prefer edit_file for partial changes.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        file_resolution(
            WriteFileRequest {
                path: path.clone(),
                content: input.content,
            },
            FileOperation::Write,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .write(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `edit_file` 标准工具壳。
pub struct FsEditTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
}

/// `edit_file` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileInput {
    /// 绝对路径或相对 Session 工作目录的目标路径。
    pub path: String,
    /// 需要精确匹配的原文本。
    pub old_string: String,
    /// 替换后的新文本。
    pub new_string: String,
    /// 是否替换全部匹配；省略时为 `false`。
    pub replace_all: Option<bool>,
}

impl FsEditTool {
    /// 用文件能力和 Session 路径解析器装配工具壳。
    pub fn new(fs: Arc<dyn FileSystemTool>, resolver: SessionPathResolver) -> Self {
        Self { fs, resolver }
    }
}

impl Tool for FsEditTool {
    type Input = EditFileInput;
    type ResolvedInput = EditFileRequest;
    type Output = EditFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("edit_file").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Replace an exact string; multiple matches require replace_all=true.".to_owned()
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new().with("replace_all", false)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.old_string.is_empty() {
            return Err(ToolError::invalid_input("old_string must not be empty"));
        }
        if input.old_string == input.new_string {
            return Err(ToolError::invalid_input(
                "old_string and new_string must differ",
            ));
        }
        let path = resolve_path(&self.resolver, &input.path)?;
        file_resolution(
            EditFileRequest {
                path: path.clone(),
                old_string: input.old_string,
                new_string: input.new_string,
                replace_all: input.replace_all.unwrap_or(false),
            },
            FileOperation::Edit,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .edit(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

/// `delete_file` 标准工具壳。
pub struct FsDeleteTool {
    fs: Arc<dyn FileSystemTool>,
    resolver: SessionPathResolver,
}

/// `delete_file` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteFileInput {
    /// 绝对路径或相对 Session 工作目录的目标路径。
    pub path: String,
}

impl FsDeleteTool {
    /// 用文件能力和 Session 路径解析器装配工具壳。
    pub fn new(fs: Arc<dyn FileSystemTool>, resolver: SessionPathResolver) -> Self {
        Self { fs, resolver }
    }
}

impl Tool for FsDeleteTool {
    type Input = DeleteFileInput;
    type ResolvedInput = DeleteFileRequest;
    type Output = DeleteFileResult;

    fn name(&self) -> ToolName {
        ToolName::new("delete_file").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Delete a file, symlink, or empty directory; deleting a symlink removes the link itself."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        file_resolution(
            DeleteFileRequest { path: path.clone() },
            FileOperation::Delete,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.fs
                .delete(input, file_context(context))
                .await
                .map_err(map_fs_error)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::{
        AbsolutePath, Dispatcher, FileEntry, FileEntryKind, FsFuture, ResolvedBatchItemRef,
        SearchMatch, ToolRegistry,
        testutil::{block_on, tool_call},
    };

    #[derive(Default)]
    struct ProbeFs {
        reads: Mutex<Vec<ReadFileRequest>>,
    }

    impl FileSystemTool for ProbeFs {
        fn read<'a>(
            &'a self,
            request: ReadFileRequest,
            context: FileToolContext,
        ) -> FsFuture<'a, ReadFileResult> {
            Box::pin(async move {
                if context.cancellation.is_cancelled() {
                    return Err(FileToolError::Cancelled);
                }
                self.reads.lock().expect("lock reads").push(request.clone());
                Ok(ReadFileResult {
                    path: request.path,
                    offset: request.offset,
                    limit: request.limit,
                    content: "1\thello".to_owned(),
                    next_offset: None,
                    truncated: false,
                })
            })
        }

        fn list<'a>(
            &'a self,
            request: ListDirectoryRequest,
            _context: FileToolContext,
        ) -> FsFuture<'a, ListDirectoryResult> {
            Box::pin(async move {
                Ok(ListDirectoryResult {
                    entries: vec![FileEntry {
                        path: request.path,
                        kind: FileEntryKind::Directory,
                        is_symlink: true,
                    }],
                })
            })
        }

        fn search<'a>(
            &'a self,
            request: SearchFilesRequest,
            _context: FileToolContext,
        ) -> FsFuture<'a, SearchFilesResult> {
            Box::pin(async move {
                Ok(SearchFilesResult {
                    matches: vec![SearchMatch::Name { path: request.path }],
                    truncated: false,
                    truncation_reason: None,
                })
            })
        }

        fn write<'a>(
            &'a self,
            request: WriteFileRequest,
            _context: FileToolContext,
        ) -> FsFuture<'a, WriteFileResult> {
            Box::pin(async move {
                Ok(WriteFileResult {
                    path: request.path,
                    bytes_written: request.content.len() as u64,
                })
            })
        }

        fn delete<'a>(
            &'a self,
            request: DeleteFileRequest,
            _context: FileToolContext,
        ) -> FsFuture<'a, DeleteFileResult> {
            Box::pin(async move {
                Ok(DeleteFileResult {
                    deleted: request.path,
                })
            })
        }

        fn edit<'a>(
            &'a self,
            request: EditFileRequest,
            _context: FileToolContext,
        ) -> FsFuture<'a, EditFileResult> {
            Box::pin(async move {
                Ok(EditFileResult {
                    path: request.path,
                    replacements: 1,
                })
            })
        }
    }

    fn nonzero(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero test value")
    }

    fn root() -> AbsolutePath {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\workspace\project");
        #[cfg(not(windows))]
        let path = PathBuf::from("/workspace/project");
        AbsolutePath::new(path).expect("test root")
    }

    fn resolver() -> SessionPathResolver {
        SessionPathResolver::new(root())
    }

    fn read_config() -> ReadFileToolConfig {
        ReadFileToolConfig::new(nonzero(1), nonzero(200), nonzero(2000)).expect("valid read config")
    }

    fn search_config() -> SearchFilesToolConfig {
        SearchFilesToolConfig::new(
            nonzero(100),
            nonzero(1000),
            NonZeroU64::new(1024 * 1024).expect("non-zero"),
            NonZeroU64::new(64 * 1024).expect("non-zero"),
        )
        .expect("valid search config")
    }

    #[test]
    fn read_schema_resolved_facts_fingerprint_and_execution_share_effective_input() {
        let fs = Arc::new(ProbeFs::default());
        let mut registry = ToolRegistry::new();
        registry
            .register(FsReadTool::new(fs.clone(), resolver(), read_config()))
            .expect("register read");
        let snapshot = registry.snapshot();
        let definition = &snapshot.definitions()[0];
        assert_eq!(
            definition.input_schema["properties"]["offset"]["default"],
            serde_json::json!(1)
        );
        assert_eq!(
            definition.input_schema["properties"]["limit"]["default"],
            serde_json::json!(200)
        );
        let mut batch = Dispatcher::resolve_batch(
            &snapshot,
            &[tool_call(
                "read_file",
                serde_json::json!({"path": "src/../README.md"}),
            )],
        );
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("read resolves");
        };
        let expected_path = root().as_path().join("README.md");
        assert_eq!(
            invocation.resolved_arguments(),
            &serde_json::json!({
                "path": expected_path.to_str().expect("utf8"),
                "offset": 1,
                "limit": 200,
            })
        );
        let facts = invocation
            .facts::<FileAuthorizationFacts>()
            .expect("file facts");
        assert_eq!(facts.operation, FileOperation::Read);
        assert_eq!(facts.path.as_path(), expected_path);
        assert_eq!(
            invocation.fingerprint().semantic_arguments()["operation"],
            serde_json::json!("read")
        );

        let result = block_on(
            Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("dispatch read"),
        );
        assert!(result.status == agent_types::ToolResultStatus::Success);
        assert_eq!(fs.reads.lock().expect("lock reads").len(), 1);
        assert_eq!(
            fs.reads.lock().expect("lock reads")[0].path.as_path(),
            expected_path
        );
    }

    #[test]
    fn search_defaults_are_visible_and_limits_are_validated_during_resolve() {
        let fs = Arc::new(ProbeFs::default());
        let tool = FsFindTool::new(fs, resolver(), search_config());
        let mut registry = ToolRegistry::new();
        registry.register(tool).expect("register find");
        let snapshot = registry.snapshot();
        let definition = &snapshot.definitions()[0];
        assert_eq!(
            definition.input_schema["properties"]["path"]["default"],
            serde_json::json!(root().as_str())
        );
        assert_eq!(
            definition.input_schema["properties"]["max_results"]["default"],
            serde_json::json!(100)
        );
        let valid = Dispatcher::resolve_batch(
            &snapshot,
            &[tool_call("find_files", serde_json::json!({"query": "rs"}))],
        );
        let Some(ResolvedBatchItemRef::Valid(invocation)) = valid.get(0) else {
            panic!("find resolves");
        };
        assert_eq!(invocation.resolved_arguments()["path"], root().as_str());
        assert_eq!(invocation.resolved_arguments()["max_results"], 100);
        assert_eq!(
            invocation.resolved_arguments()["max_output_bytes"],
            1024 * 1024
        );
        assert_eq!(
            invocation.resolved_arguments()["max_record_bytes"],
            64 * 1024
        );

        let invalid = Dispatcher::resolve_batch(
            &snapshot,
            &[tool_call(
                "find_files",
                serde_json::json!({"query": "rs", "max_results": 1001}),
            )],
        );
        assert!(matches!(
            invalid.get(0),
            Some(ResolvedBatchItemRef::Invalid(_))
        ));
    }

    #[test]
    fn empty_paths_queries_and_invalid_edit_are_rejected_without_execution() {
        let fs = Arc::new(ProbeFs::default());
        assert!(matches!(
            FsListTool::new(fs.clone(), resolver()).resolve(ListDirectoryInput {
                path: String::new(),
            }),
            Err(ToolError::InvalidInput { .. })
        ));
        assert!(matches!(
            FsSearchTool::new(fs.clone(), resolver(), search_config()).resolve(
                SearchContentInput {
                    query: String::new(),
                    path: None,
                    max_results: None,
                }
            ),
            Err(ToolError::InvalidInput { .. })
        ));
        assert!(matches!(
            FsEditTool::new(fs, resolver()).resolve(EditFileInput {
                path: "a.txt".to_owned(),
                old_string: "same".to_owned(),
                new_string: "same".to_owned(),
                replace_all: None,
            }),
            Err(ToolError::InvalidInput { .. })
        ));
    }

    #[test]
    fn invalid_instance_configs_fail_before_tool_construction() {
        assert!(ReadFileToolConfig::new(nonzero(1), nonzero(20), nonzero(10)).is_err());
        assert!(
            SearchFilesToolConfig::new(
                nonzero(20),
                nonzero(10),
                NonZeroU64::new(1024).expect("non-zero"),
                NonZeroU64::new(512).expect("non-zero"),
            )
            .is_err()
        );
    }

    #[test]
    fn cancellation_is_forwarded_to_file_capability() {
        let fs = Arc::new(ProbeFs::default());
        let tool = FsReadTool::new(fs, resolver(), read_config());
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let resolution = tool
            .resolve(ReadFileInput {
                path: "README.md".to_owned(),
                offset: None,
                limit: None,
            })
            .expect("resolve read");
        let error = block_on(tool.execute(
            resolution.into_input(),
            ToolContext::new(cancellation, Arc::new(|_| {})),
        ))
        .expect_err("cancelled capability fails");
        assert!(matches!(error, ToolError::Execution { .. }));
    }

    #[test]
    fn mutation_tools_resolve_then_execute_through_dispatcher() {
        let fs = Arc::new(ProbeFs::default());
        let mut registry = ToolRegistry::new();
        registry
            .register(FsWriteTool::new(fs.clone(), resolver()))
            .expect("register write");
        registry
            .register(FsEditTool::new(fs.clone(), resolver()))
            .expect("register edit");
        registry
            .register(FsDeleteTool::new(fs, resolver()))
            .expect("register delete");
        let snapshot = registry.snapshot();
        let calls = [
            tool_call(
                "write_file",
                serde_json::json!({"path": "notes.txt", "content": "hello"}),
            ),
            tool_call(
                "edit_file",
                serde_json::json!({
                    "path": "notes.txt",
                    "old_string": "hello",
                    "new_string": "world",
                    "replace_all": false
                }),
            ),
            tool_call("delete_file", serde_json::json!({"path": "notes.txt"})),
        ];
        let mut batch = Dispatcher::resolve_batch(&snapshot, &calls);
        for (index, operation) in [
            FileOperation::Write,
            FileOperation::Edit,
            FileOperation::Delete,
        ]
        .into_iter()
        .enumerate()
        {
            let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(index) else {
                panic!("mutation resolves");
            };
            assert_eq!(
                invocation
                    .facts::<FileAuthorizationFacts>()
                    .expect("file facts")
                    .operation,
                operation
            );
            let result = block_on(
                Dispatcher::execute(&mut batch, index, ToolContext::default())
                    .expect("valid mutation index"),
            );
            assert_eq!(result.status, agent_types::ToolResultStatus::Success);
        }
    }
}
