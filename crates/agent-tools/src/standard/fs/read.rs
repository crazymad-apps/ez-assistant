//! 文件读取与目录枚举工具壳。

use std::{num::NonZeroU32, sync::Arc};

use agent_types::ToolName;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{
        FileOperation, FileSystemTool, ListDirectoryRequest, ListDirectoryResult, ReadFileRequest,
        ReadFileResult,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults},
};

use super::{file_context, file_resolution, map_fs_error, resolve_path};
use crate::standard::ToolConfigurationError;

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
