//! 文件写入、局部编辑与删除工具壳。

use std::sync::Arc;

use agent_types::{ToolName, ToolResultStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{
        DeleteFileRequest, DeleteFileResult, EditFileEntry, EditFileOutcome, EditFileRequest,
        EditFileResult, EditReplacement, FileBatchAuthorizationFacts, FileOperation,
        FileSystemTool, MAX_EDIT_INPUT_BYTES, WriteFileRequest, WriteFileResult,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture},
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

/// 推荐批量输入与已发布平面输入互斥；两者均进入唯一的冻结请求。
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
#[schemars(transform = exclusive_edit_forms)]
pub enum EditFileInput {
    Batch {
        #[schemars(length(min = 1, max = 16))]
        files: Vec<EditFileInputEntry>,
    },
    Legacy {
        path: String,
        old_string: String,
        new_string: String,
        #[schemars(default = "default_replace_all")]
        replace_all: Option<bool>,
    },
}

/// 模型批量输入中的单文件目标，路径尚未解析。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileInputEntry {
    pub path: String,
    #[schemars(length(min = 1, max = 64))]
    pub edits: Vec<EditReplacement>,
}

// 两个封闭且互斥的对象分支采用 oneOf，让现有 Provider 子集转换复用对象合并。
fn exclusive_edit_forms(schema: &mut schemars::Schema) {
    if let Some(forms) = schema.remove("anyOf") {
        schema.insert("oneOf".to_owned(), forms);
    }
}

fn default_replace_all() -> Option<bool> {
    Some(false)
}

impl<'de> Deserialize<'de> for EditFileInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 在默认值和绝对路径展开前核对原始紧凑 JSON 的真实字节预算。
        let value = serde_json::Value::deserialize(deserializer)?;
        if serde_json::to_vec(&value)
            .map_err(serde::de::Error::custom)?
            .len()
            > MAX_EDIT_INPUT_BYTES
        {
            return Err(serde::de::Error::custom("edit input exceeds 256 KiB"));
        }
        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum Wire {
            Batch {
                files: Vec<EditFileInputEntry>,
            },
            Legacy {
                path: String,
                old_string: String,
                new_string: String,
                #[serde(default = "default_replace_all")]
                replace_all: Option<bool>,
            },
        }
        match serde_json::from_value::<Wire>(value).map_err(serde::de::Error::custom)? {
            Wire::Batch { files } => Ok(Self::Batch { files }),
            Wire::Legacy {
                path,
                old_string,
                new_string,
                replace_all,
            } => Ok(Self::Legacy {
                path,
                old_string,
                new_string,
                replace_all,
            }),
        }
    }
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
        "Edit UTF-8 files with ordered exact replacements: files[{path,edits[{old_string,new_string,replace_all=false}]}]. Legacy path/old_string/new_string is also accepted; never mix forms. Limits: 16 files, 64 edits/file, 128 edits total, 256 KiB JSON; resolved paths at most 1 KiB each and 4 KiB total (8 KiB encoded). Multiple matches require replace_all=true. Each file is validated fully before commit; the batch stops on failure/cancellation and reports success/failed/unknown/not_executed without rollback.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let files = match input {
            EditFileInput::Batch { files } => files,
            EditFileInput::Legacy {
                path,
                old_string,
                new_string,
                replace_all,
            } => vec![EditFileInputEntry {
                path,
                edits: vec![EditReplacement {
                    old_string,
                    new_string,
                    replace_all: replace_all.unwrap_or(false),
                }],
            }],
        };
        let request = EditFileRequest {
            files: files
                .into_iter()
                .map(|file| {
                    Ok(EditFileEntry {
                        path: resolve_path(&self.resolver, &file.path)?,
                        edits: file.edits,
                    })
                })
                .collect::<Result<_, ToolError>>()?,
        };
        request.validate().map_err(map_fs_error)?;
        let paths = request.files.iter().map(|file| file.path.clone()).collect();
        let mut arguments = serde_json::to_value(&request)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        arguments["operation"] = serde_json::to_value(FileOperation::Edit)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        Ok(ToolResolution::with_facts(
            request,
            FileBatchAuthorizationFacts {
                operation: FileOperation::Edit,
                paths,
            },
            arguments,
        ))
    }

    fn output_status(output: &Self::Output) -> ToolResultStatus {
        if !output.files.is_empty()
            && output
                .files
                .iter()
                .all(|file| matches!(file.outcome, EditFileOutcome::Success { .. }))
        {
            ToolResultStatus::Success
        } else {
            ToolResultStatus::Error
        }
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
