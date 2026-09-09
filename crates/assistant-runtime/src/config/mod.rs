//! Runtime 全局策略、数据库模型执行编译和配置源边界。
//! schema/compile 只解释全局 TOML；managed_model 编译在线或固定参数；registry 是配置唯一 owner。

mod capabilities;
mod compile;
mod domain;
mod managed_model;
pub(crate) mod model_templates;
mod protocol;
mod registry;
mod schema;
mod source;

use std::{num::NonZeroUsize, time::Duration};

pub use capabilities::{
    ReasoningEffortKey, ReasoningEffortWireValue, ResolvedModelCapabilities,
    ResolvedReasoningCapability, ResolvedReasoningEffort,
};
pub use compile::compile_runtime_config;
pub use domain::{
    ConfigCompilation, ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigState,
    DelegationConfig, McpRuntimeConfig, ModelProtocol, ResolvedConfig, ResolvedModelConfig,
    RuntimeModelTransportConfig,
};
pub use source::{
    ConfigDocument, ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture,
    ConfigSourceLoad, ConfigSourceReplace, ConfigSourceReplaceFuture, RuntimeConfigSource,
};

pub(crate) use managed_model::{PreparedModel, resolve_provider_protocol};
pub(crate) use protocol::project_status;
pub(crate) use registry::{ConfigRegistry, ConfigSnapshot, ManagedModels};

/// Assistant Runtime 初始化版本所需的最小进程内配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Runtime 实时观察事件通道容量；M4 建立 Event Hub 时使用。
    pub event_capacity: NonZeroUsize,
    /// 受控关闭等待 Runtime 所有 supervisor 优雅退出的最长时间。
    pub shutdown_timeout: Duration,
}

impl RuntimeConfig {
    const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

    /// 使用显式的有界事件容量创建配置。
    pub fn new(event_capacity: NonZeroUsize) -> Self {
        Self {
            event_capacity,
            shutdown_timeout: Self::DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// 显式覆盖受控关闭上限；主要用于宿主策略和确定性测试。
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }
}
