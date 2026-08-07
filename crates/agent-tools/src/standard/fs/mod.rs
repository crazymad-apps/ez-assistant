//! 标准文件工具壳：把模型输入解析为绝对逻辑路径、有效默认值与类型化授权事实。
//!
//! resolve 阶段只做纯计算；文件是否存在、符号链接和真实读写全部委托给注入的
//! [`FileSystemTool`]。

mod read;
mod search;
mod write;

pub use read::{FsListTool, FsReadTool, ListDirectoryInput, ReadFileInput, ReadFileToolConfig};
pub use search::{
    FindFilesInput, FsFindTool, FsSearchTool, SearchContentInput, SearchFilesToolConfig,
};
pub use write::{
    DeleteFileInput, EditFileInput, FsDeleteTool, FsEditTool, FsWriteTool, WriteFileInput,
};

use serde::Serialize;
use serde_json::Value;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::fs::{FileAuthorizationFacts, FileOperation, FileToolContext, FileToolError},
    tool::{ToolContext, ToolError},
};

pub(super) fn map_fs_error(error: FileToolError) -> ToolError {
    match error {
        FileToolError::InvalidInput { message } => ToolError::invalid_input(message),
        // Engine 在执行取消后会直接收敛到 Cancelled，不会把映射后的文本回喂模型；
        // Cancelled 落入此分支只为能力错误到通用 ToolError 的类型映射保持完备。
        other => ToolError::execution(other.to_string()),
    }
}

pub(super) fn resolve_path(
    resolver: &SessionPathResolver,
    input: &str,
) -> Result<crate::AbsolutePath, ToolError> {
    resolver
        .resolve(input)
        .map_err(|error| ToolError::invalid_input(error.to_string()))
}

pub(super) fn file_resolution<T: Serialize>(
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

pub(super) fn file_context(context: ToolContext) -> FileToolContext {
    FileToolContext::new(context.cancellation)
}

#[cfg(test)]
mod tests;
