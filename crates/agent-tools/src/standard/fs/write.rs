//! 文件写入、局部编辑与删除工具壳。

use std::sync::Arc;

use agent_types::ToolName;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{
        DeleteFileRequest, DeleteFileResult, EditFileRequest, EditFileResult, FileOperation,
        FileSystemTool, WriteFileRequest, WriteFileResult,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults},
};

use super::{file_context, file_resolution, map_fs_error, resolve_path};

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
