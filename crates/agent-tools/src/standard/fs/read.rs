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
    default_offset_chars: u64,
    default_limit_chars: NonZeroU32,
    maximum_limit_chars: NonZeroU32,
}

impl ReadFileToolConfig {
    /// 创建读取配置；默认单页字符数不得超过实例允许的最大字符数。
    pub fn new(
        default_offset_chars: u64,
        default_limit_chars: NonZeroU32,
        maximum_limit_chars: NonZeroU32,
    ) -> Result<Self, ToolConfigurationError> {
        if default_limit_chars > maximum_limit_chars {
            return Err(ToolConfigurationError::new(
                "read default_limit_chars must not exceed maximum_limit_chars",
            ));
        }
        Ok(Self {
            default_offset_chars,
            default_limit_chars,
            maximum_limit_chars,
        })
    }

    /// 未传 `offset_chars` 时采用的 0 起始字符偏移。
    pub fn default_offset_chars(&self) -> u64 {
        self.default_offset_chars
    }

    /// 未传 `limit_chars` 时采用的单页字符数。
    pub fn default_limit_chars(&self) -> NonZeroU32 {
        self.default_limit_chars
    }

    /// 单次模型调用允许请求的最大字符数。
    pub fn maximum_limit_chars(&self) -> NonZeroU32 {
        self.maximum_limit_chars
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
    /// 0 起始的 Unicode scalar value 偏移（不是字节、UTF-16 单元或行号）；续读传 `next_offset_chars`。
    pub offset_chars: Option<u64>,
    /// 本页最多返回的 Unicode scalar value 数量；不得超过工具描述中的 maximum limit_chars。
    pub limit_chars: Option<NonZeroU32>,
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
            "Read a UTF-8 text file by Unicode scalar values, not bytes, UTF-16 units or lines. \
             Return at most limit_chars characters, including newlines, even within a single long line. \
             Content is unchanged; start_line/start_column are 1-based location hints. Calls are stateless: \
             repeating the same path and arguments returns the same page and never advances automatically. \
             To continue reading, pass the previous result's next_offset_chars as offset_chars; null means EOF. \
             Each call reads the current file. Default offset_chars: {}; default limit_chars: {}; \
             maximum limit_chars: {}. Larger limits are rejected. Legacy offset/limit line arguments are unsupported.",
            self.config.default_offset_chars,
            self.config.default_limit_chars,
            self.config.maximum_limit_chars,
        )
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("offset_chars", self.config.default_offset_chars)
            .with("limit_chars", self.config.default_limit_chars)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        let limit_chars = input.limit_chars.unwrap_or(self.config.default_limit_chars);
        if limit_chars > self.config.maximum_limit_chars {
            return Err(ToolError::invalid_input(format!(
                "limit_chars must not exceed {}",
                self.config.maximum_limit_chars
            )));
        }
        file_resolution(
            ReadFileRequest {
                path: path.clone(),
                offset_chars: input
                    .offset_chars
                    .unwrap_or(self.config.default_offset_chars),
                limit_chars,
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
