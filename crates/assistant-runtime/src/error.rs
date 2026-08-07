//! Runtime library 的结构化错误及应用协议映射。

use agent_sdk::AgentBuildError;
use assistant_protocol::{
    ModelKey, RunId, RuntimeErrorCode, RuntimeErrorInfo, RuntimeLifecycle, SessionId,
};
use thiserror::Error;

use crate::{ModelServiceFactoryError, SystemPromptFactoryError};

/// Runtime 操作失败。
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// 客户端提交的业务字段不满足 Runtime 约束。
    #[error("runtime request is invalid: {reason}")]
    InvalidRequest {
        /// 已脱敏、可安全展示的原因。
        reason: &'static str,
    },
    /// 当前生命周期不接受新的变更操作。
    #[error("runtime is not running: {lifecycle:?}")]
    RuntimeNotRunning {
        /// 拒绝操作时观察到的生命周期。
        lifecycle: RuntimeLifecycle,
    },
    /// 目标 Session 不存在。
    #[error("session `{session_id}` was not found")]
    SessionNotFound {
        /// 未找到的 Session。
        session_id: SessionId,
    },
    /// Session 已有活动 Run 或存在未完成结算。
    #[error("session `{session_id}` is busy")]
    SessionBusy {
        /// 当前无法接受新 Run 的 Session。
        session_id: SessionId,
    },
    /// 目标 Run 不存在于指定 Session。
    #[error("run `{run_id}` was not found in session `{session_id}`")]
    RunNotFound {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 未找到的 Run。
        run_id: RunId,
    },
    /// Session 内部结算不变量已被破坏，后续变更被拒绝。
    #[error("session `{session_id}` is faulted")]
    SessionFaulted {
        /// 发生内部故障的 Session。
        session_id: SessionId,
    },
    /// 当前配置没有可供业务使用的 active 快照。
    #[error("runtime configuration is unavailable")]
    ConfigurationUnavailable,
    /// 用户指定的模型 key 不存在。
    #[error("model `{model_key}` was not found")]
    ModelNotFound { model_key: ModelKey },
    /// 模型条目存在但当前静态配置无效。
    #[error("model `{model_key}` is unavailable")]
    ModelUnavailable { model_key: ModelKey },
    /// Host 无法从已校验配置构造具体模型服务。
    #[error("model service could not be created")]
    ModelBuildFailed {
        #[source]
        source: ModelServiceFactoryError,
    },
    /// System Prompt 在 Session 入库前构造失败。
    #[error("system prompt could not be created")]
    SystemPromptBuildFailed {
        #[source]
        source: SystemPromptFactoryError,
    },
    /// Run Agent 构造失败，UserMessage 尚未写入。
    #[error("run agent could not be created")]
    AgentBuildFailed {
        #[source]
        source: AgentBuildError,
    },
    /// 锁中毒、标识耗尽或其他不应向客户端暴露细节的内部错误。
    #[error("runtime internal state is unavailable: {component}")]
    InternalStateUnavailable {
        /// 仅用于本地日志定位的组件名称，不包含用户或 Provider 数据。
        component: &'static str,
    },
}

impl RuntimeError {
    /// 转换为 Host 可以安全发送给客户端的稳定错误信息。
    pub fn to_protocol_info(&self) -> RuntimeErrorInfo {
        match self {
            Self::InvalidRequest { reason } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::InvalidRequest, *reason)
            }
            Self::RuntimeNotRunning { lifecycle } => RuntimeErrorInfo::new(
                RuntimeErrorCode::RuntimeShuttingDown,
                match lifecycle {
                    RuntimeLifecycle::Running => "runtime is unavailable",
                    RuntimeLifecycle::ShuttingDown => "runtime is shutting down",
                    RuntimeLifecycle::Stopped => "runtime has stopped",
                },
            ),
            Self::SessionNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::SessionNotFound, "session was not found")
            }
            Self::SessionBusy { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::SessionBusy, "session is busy")
            }
            Self::RunNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::RunNotFound, "run was not found")
            }
            Self::SessionFaulted { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "session internal state is unavailable",
            ),
            Self::ConfigurationUnavailable => RuntimeErrorInfo::new(
                RuntimeErrorCode::ConfigurationUnavailable,
                "runtime configuration is unavailable",
            ),
            Self::ModelNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::ModelNotFound, "model was not found")
            }
            Self::ModelUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ModelUnavailable,
                "model configuration is unavailable",
            ),
            Self::ModelBuildFailed { .. } | Self::AgentBuildFailed { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ModelBuildFailed,
                "model could not be prepared for this run",
            ),
            Self::SystemPromptBuildFailed { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::AgentBuildFailed,
                "system prompt could not be created",
            ),
            Self::InternalStateUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "runtime internal state is unavailable",
            ),
        }
    }
}

/// Runtime 公共操作的统一结果类型。
pub type RuntimeResult<T> = Result<T, RuntimeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_mapping_does_not_leak_internal_component_or_factory_message() {
        let internal = RuntimeError::InternalStateUnavailable {
            component: "sessions lock",
        };
        let info = internal.to_protocol_info();
        assert_eq!(info.code, RuntimeErrorCode::Internal);
        assert!(!info.message.contains("sessions lock"));

        let factory = RuntimeError::ModelBuildFailed {
            source: ModelServiceFactoryError::new("model fixture failed"),
        };
        let info = factory.to_protocol_info();
        assert_eq!(info.code, RuntimeErrorCode::ModelBuildFailed);
        assert!(!info.message.contains("sk-private"));
    }
}
