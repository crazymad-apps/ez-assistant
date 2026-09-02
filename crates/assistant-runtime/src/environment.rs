//! Session 创建时一次性冻结的目录环境及宿主构造边界。

use std::error::Error;

use agent_model::SystemPromptSnapshot;
use assistant_protocol::{SessionId, WorkspaceId};
use thiserror::Error;

/// 一个 Session 后续运行、附件和临时文件共同使用的不可变目录事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionExecutionEnvironment {
    /// 创建时冻结的可选 Workspace 绑定。
    pub workspace_id: Option<WorkspaceId>,
    /// Agent 默认工作的用户目录；未绑定 Workspace 时指向 Session 私有目录。
    pub working_directory: String,
    /// 创建 Session 时冻结的有序 Workspace 附加目录；后续 Workspace 编辑不会改写。
    pub additional_workspace_directories: Vec<String>,
    /// Workspace 范围、可由多个 Session 复用的 Agent 私有目录。
    pub workspace_private_directory: Option<String>,
    /// Session 范围、持久保存上传附件的静态目录。
    pub session_attachment_directory: String,
    /// Session 范围、由 Runtime 管理的稳定工具图片目录。
    pub session_tool_image_directory: String,
    /// Session 范围、供 Agent 保存会话私有文件的持久目录。
    pub session_private_directory: String,
}

/// Environment Factory 构造绑定环境时需要的稳定 Workspace 投影。
pub struct WorkspaceEnvironmentSource<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub label: &'a str,
    pub user_directory: &'a str,
    pub additional_directories: &'a [String],
    pub agent_directory: &'a str,
}

/// Session 创建时交给宿主 Factory 的冻结输入。
pub struct SessionEnvironmentFactoryRequest<'a> {
    pub session_id: &'a SessionId,
    pub workspace: Option<WorkspaceEnvironmentSource<'a>>,
    pub memory_context: &'a crate::MemoryContextSnapshot,
}

/// Fork 只继承来源冻结前缀并重建新 Session 自有目录。
pub struct ForkSessionEnvironmentFactoryRequest<'a> {
    pub session_id: &'a SessionId,
    pub source_system_prompt: &'a SystemPromptSnapshot,
    pub source_environment: &'a SessionExecutionEnvironment,
}

/// 一次构造同时产生持久化 System Prompt 和目录环境，避免二者发生偏移。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedSessionEnvironment {
    pub system_prompt: SystemPromptSnapshot,
    pub environment: SessionExecutionEnvironment,
}

/// Session 环境或 System Prompt 构造失败。
#[derive(Debug, Error)]
#[error("session environment could not be created")]
pub struct SessionEnvironmentFactoryError {
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl SessionEnvironmentFactoryError {
    pub fn new() -> Self {
        Self { source: None }
    }

    pub fn with_source(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
    }
}

impl Default for SessionEnvironmentFactoryError {
    fn default() -> Self {
        Self::new()
    }
}

/// Host 在 Session 入库前一次性生成稳定路径和冻结 System Prompt。
pub trait SessionEnvironmentFactory: Send + Sync {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError>;

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError>;
}
