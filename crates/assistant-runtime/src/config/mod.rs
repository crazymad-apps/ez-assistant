//! Runtime 构造配置与用户模型配置的解析、编译边界。
//!
//! `schema` 只接收 TOML 形状，`compile` 处理全局状态，`model` 隔离逐模型错误，`domain`
//! 分开执行快照与安全投影。`source`/`registry` 管理读取边界和 reload 生命周期；真实文件权限
//! 检查仍属于生产 Host，不能进入纯编译模块。

mod compile;
mod domain;
mod editor;
mod model;
mod protocol;
mod registry;
mod schema;
mod source;

use std::{num::NonZeroUsize, time::Duration};

pub use compile::compile_runtime_config;
pub use domain::{
    ConfigCompilation, ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigState,
    DelegationConfig, ModelConfigProjection, ModelProtocol, ResolvedConfig, ResolvedModelConfig,
    RuntimeModelTransportConfig,
};
pub use source::{
    ConfigDocument, ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture,
    ConfigSourceLoad, ConfigSourceReplace, ConfigSourceReplaceFuture, RuntimeConfigSource,
};

pub(crate) use editor::{ConfigMutation, edit_config_document};
pub(crate) use protocol::{project_model_by_key, project_models, project_status};
pub(crate) use registry::{ConfigRegistry, ConfigSnapshot};

#[cfg(test)]
mod tests;

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
