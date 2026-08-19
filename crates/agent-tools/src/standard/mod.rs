//! 标准工具壳定义。
//!
//! 工具壳固定模型可见契约和 resolved 语义，执行则委托给装配时
//! 注入的能力实现；本地、远程或测试实现都可以实现同一能力契约。

pub mod fs;
pub mod inspect_images;
pub mod pinned_memory;
pub mod recall_memory;
pub mod shell;

/// 标准工具实例配置不自洽，无法形成稳定模型定义或 resolved 请求。
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
#[error("invalid standard tool configuration: {message}")]
pub struct ToolConfigurationError {
    /// 可操作的配置错误说明。
    pub message: String,
}

impl ToolConfigurationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
