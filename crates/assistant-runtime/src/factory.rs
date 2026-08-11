//! Run 前模型服务与 System Prompt 的宿主注入边界。

use std::{error::Error, sync::Arc, time::Duration};

use agent_core::ToolAuthorizer;
use agent_model::ModelService;
use agent_tools::ToolSetSnapshot;
use agent_types::ProviderId;
use thiserror::Error;

use crate::SessionExecutionEnvironment;

/// Runtime 已根据 provider 推导出的内部 Codec 方言。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCompatibilityProfile {
    /// DeepSeek thinking-enabled Chat Completions 方言。
    DeepSeek,
    /// 没有额外供应商行为的标准 Chat Completions 方言。
    Standard,
}

/// 交给具体 Provider 工厂的一次冻结模型构造输入。
///
/// 本类型不实现 `Debug`，因为它短暂借用 API Key。工厂不得保存借用或输出 credential。
pub struct ModelServiceFactoryRequest<'a> {
    pub provider: &'a ProviderId,
    pub profile: ModelCompatibilityProfile,
    pub endpoint: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub context_window_tokens: u64,
    pub connect_timeout: Duration,
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
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError>;
}

/// 一次 Run 冻结使用的工具定义与授权闸。
///
/// Bundle 不进入 Protocol 或 Conversation；不同 Run 必须分别由
/// [`RunToolFactory`] 编译，不得修改共享的可变路径解析器。
pub struct RunToolBundle {
    tools: ToolSetSnapshot,
    authorizer: Arc<dyn ToolAuthorizer>,
}

impl RunToolBundle {
    pub fn new(tools: ToolSetSnapshot, authorizer: Arc<dyn ToolAuthorizer>) -> Self {
        Self { tools, authorizer }
    }

    /// 消费 Bundle，取回本 Run 的不可变工具集和授权闸。
    pub fn into_parts(self) -> (ToolSetSnapshot, Arc<dyn ToolAuthorizer>) {
        (self.tools, self.authorizer)
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
        environment: &SessionExecutionEnvironment,
    ) -> Result<RunToolBundle, RunToolFactoryError>;
}
