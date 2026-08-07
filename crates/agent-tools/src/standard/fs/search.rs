//! 文件名与文件内容搜索工具壳。

use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use agent_types::ToolName;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{
        FileOperation, FileSystemTool, SearchFilesRequest, SearchFilesResult, SearchKind,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults},
};

use super::{file_context, file_resolution, map_fs_error, resolve_path};
use crate::standard::ToolConfigurationError;

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
