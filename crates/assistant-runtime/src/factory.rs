//! Run 前模型服务与 System Prompt 的宿主注入边界。

use std::{error::Error, sync::Arc, time::Duration};

use agent_model::{ModelService, SystemPromptSnapshot};
use agent_types::ProviderId;
use thiserror::Error;

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

/// System Prompt 构造失败。
#[derive(Debug, Error)]
#[error("system prompt could not be created")]
pub struct SystemPromptFactoryError {
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl SystemPromptFactoryError {
    /// 创建一条不携带底层详情的失败。
    pub fn new() -> Self {
        Self { source: None }
    }

    /// 保留底层错误链，但不把详情写入 Display。
    pub fn with_source(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
    }
}

impl Default for SystemPromptFactoryError {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 Session 创建时渲染一次冻结的 System Prompt。
pub trait SystemPromptFactory: Send + Sync {
    fn create_system_prompt(&self) -> Result<SystemPromptSnapshot, SystemPromptFactoryError>;
}
