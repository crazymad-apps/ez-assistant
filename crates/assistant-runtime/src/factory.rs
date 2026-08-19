//! Run 前模型服务与 System Prompt 的宿主注入边界。

use std::{error::Error, future::Future, pin::Pin, sync::Arc, time::Duration};

use agent_core::ToolPolicy;
use agent_memory::{MemoryRecall, PinnedMemoryStore, RecallReferenceReader};
use agent_model::ModelServiceBundle;
use agent_tools::ToolSetSnapshot;
use agent_types::ProviderId;
use thiserror::Error;

use crate::{ModelProtocol, ResolvedModelCapabilities, SessionExecutionEnvironment};

/// Host 异步创建单个子任务临时空间的结果 Future。
pub type ChildTaskWorkspaceFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<Box<dyn ChildTaskWorkspaceLease>, ChildTaskWorkspaceError>>
            + Send
            + 'a,
    >,
>;

/// 子任务活动期间独占的临时空间 lease。
///
/// Runtime 只读取稳定绝对路径；真正的目录创建与 Drop 清理由 Host 实现。
pub trait ChildTaskWorkspaceLease: Send {
    fn path(&self) -> &str;
}

/// 子任务临时空间创建失败。
#[derive(Debug, Error)]
#[error("child task workspace could not be created")]
pub struct ChildTaskWorkspaceError {
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ChildTaskWorkspaceError {
    pub fn new() -> Self {
        Self { source: None }
    }

    pub fn with_source(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
    }
}

impl Default for ChildTaskWorkspaceError {
    fn default() -> Self {
        Self::new()
    }
}

/// Host 提供的 OS 临时目录边界；Runtime 不直接访问文件系统。
pub trait ChildTaskWorkspaceFactory: Send + Sync {
    fn create<'a>(
        &'a self,
        child_task_id: &'a assistant_protocol::ChildTaskId,
    ) -> ChildTaskWorkspaceFuture<'a>;
}

/// 交给具体 Provider 工厂的一次冻结模型构造输入。
///
/// 本类型不实现 `Debug`，因为它短暂借用 API Key。工厂不得保存借用或输出 credential。
pub struct ModelServiceFactoryRequest<'a> {
    pub provider: &'a ProviderId,
    pub protocol: ModelProtocol,
    pub capabilities: &'a ResolvedModelCapabilities,
    pub endpoint: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub context_window_tokens: u64,
    pub connect_timeout: Duration,
    /// 等待响应建立及相邻流 chunk 的最长时间；不是流式请求总预算。
    pub request_timeout: Duration,
}

/// 具体 ModelService 构造失败。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ModelServiceFactoryError {
    message: &'static str,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ModelServiceFactoryError {
    /// 创建不携带底层错误的脱敏失败。
    pub fn new(message: &'static str) -> Self {
        Self {
            message,
            source: None,
        }
    }

    /// 保留底层错误链，但 Display 只使用固定安全消息。
    pub fn with_source(message: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message,
            source: Some(Box::new(source)),
        }
    }
}

/// 从 Runtime 已校验配置构造具体 Provider 模型服务。
pub trait ModelServiceFactory: Send + Sync {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError>;
}

/// 一次 Run 冻结使用的工具定义与 Host 基础设施策略。
///
/// Bundle 不进入 Protocol 或 Conversation；不同 Run 必须分别由
/// [`RunToolFactory`] 编译，不得修改共享的可变路径解析器。
pub struct RunToolBundle {
    tools: ToolSetSnapshot,
    infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
}

/// Host 编译单次 Run 工具所需的 Runtime 绑定能力。
pub struct RunToolFactoryRequest<'a> {
    pub session_id: &'a assistant_protocol::SessionId,
    pub environment: &'a SessionExecutionEnvironment,
    pub pinned_memory: Arc<dyn PinnedMemoryStore>,
    pub conversation_recall: Arc<dyn MemoryRecall>,
    pub conversation_recall_reader: Arc<dyn RecallReferenceReader>,
    /// Runtime 已按主/辅助模型能力判定后的可选识图能力。
    pub image_inspector: Option<agent_tools::SharedImageInspector>,
}

impl RunToolBundle {
    pub fn new(tools: ToolSetSnapshot, infrastructure_policies: Vec<Arc<dyn ToolPolicy>>) -> Self {
        Self {
            tools,
            infrastructure_policies,
        }
    }

    /// 消费 Bundle，取回本 Run 的不可变工具集和 Host 基础设施策略。
    pub fn into_parts(self) -> (ToolSetSnapshot, Vec<Arc<dyn ToolPolicy>>) {
        (self.tools, self.infrastructure_policies)
    }
}

/// Host 构造单次 Run 工具时的失败分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunToolFactoryErrorKind {
    /// Session 冻结的默认工作目录当前不可用。
    WorkingDirectoryUnavailable,
    /// 路径、工具配置或注册无法形成不可变 Bundle。
    InvalidConfiguration,
}

/// 单次 Run 工具编译失败。
#[derive(Debug, Error)]
#[error("run tools could not be created")]
pub struct RunToolFactoryError {
    kind: RunToolFactoryErrorKind,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl RunToolFactoryError {
    pub fn new(kind: RunToolFactoryErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: RunToolFactoryErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    pub fn kind(&self) -> RunToolFactoryErrorKind {
        self.kind
    }
}

/// 根据 Session 创建时冻结的目录事实编译单次 Run 工具。
pub trait RunToolFactory: Send + Sync {
    fn compile(
        &self,
        request: RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError>;
}
