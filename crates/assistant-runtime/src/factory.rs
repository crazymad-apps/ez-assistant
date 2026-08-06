//! 每 Session 一个冻结 Agent 的构造边界。

use std::error::Error;

use agent_sdk::Agent;
use thiserror::Error;

/// Session Agent 构造失败。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct AgentFactoryError {
    message: String,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl AgentFactoryError {
    /// 创建一条不携带底层错误的脱敏构造错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// 创建一条保留底层错误链、但只以给定安全消息展示的构造错误。
    pub fn with_source(
        message: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// 返回允许写入普通 Runtime 错误上下文的安全消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 为每个新 Session 构造独立冻结 Agent 的同步工厂。
///
/// 模型、Prompt、Context Window 和工具集合应在 Host 启动时准备完成；工厂不读取或保存动态
/// Conversation，也不能把一个 Agent 实例复用于多个 Session。
pub trait SessionAgentFactory: Send + Sync {
    /// 构造一个只属于新 Session 的 Agent。
    fn create_agent(&self) -> Result<Agent, AgentFactoryError>;
}
