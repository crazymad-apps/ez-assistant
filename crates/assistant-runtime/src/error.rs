//! Runtime library 的结构化错误及应用协议映射。

use agent_sdk::AgentBuildError;
use assistant_protocol::{
    ApprovalId, AttachmentId, ChildTaskId, InputId, ModelKey, RunId, RuntimeErrorCode,
    RuntimeErrorInfo, RuntimeLifecycle, SessionId, WorkspaceId,
};
use thiserror::Error;

use crate::{
    ModelServiceFactoryError, RunToolFactoryError, SessionEnvironmentFactoryError, StoreError,
};

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
    /// 归档 Session 只允许只读查询。
    #[error("session `{session_id}` is archived")]
    SessionArchived { session_id: SessionId },
    /// Session 尚有活动、排队或未终结 Run。
    #[error("session `{session_id}` is not idle")]
    SessionNotIdle { session_id: SessionId },
    /// 目标 Run 不存在于指定 Session。
    #[error("run `{run_id}` was not found in session `{session_id}`")]
    RunNotFound {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 未找到的 Run。
        run_id: RunId,
    },
    #[error("child task `{child_task_id}` was not found in session `{session_id}`")]
    ChildTaskNotFound {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
    #[error("input `{input_id}` was not found in session `{session_id}`")]
    InputNotFound {
        session_id: SessionId,
        input_id: InputId,
    },
    #[error("run `{run_id}` is not retryable in session `{session_id}`")]
    RunNotRetryable {
        session_id: SessionId,
        run_id: RunId,
    },
    /// Session 内部结算不变量已被破坏，后续变更被拒绝。
    #[error("session `{session_id}` is faulted")]
    SessionFaulted {
        /// 发生内部故障的 Session。
        session_id: SessionId,
    },
    /// Runtime 的权威存储无法完成本次业务操作。
    #[error("runtime storage is unavailable while attempting to {operation}")]
    StorageUnavailable {
        /// 不包含路径、正文或凭证的稳定操作名称。
        operation: &'static str,
        /// Host Store 保留的进程内诊断来源。
        #[source]
        source: Option<StoreError>,
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
    /// Host 无法基于 Session 冻结环境编译单次 Run 工具。
    #[error("run tools could not be created")]
    RunToolsBuildFailed {
        #[source]
        source: RunToolFactoryError,
    },
    /// Session 环境或 System Prompt 在入库前构造失败。
    #[error("session environment could not be created")]
    SessionEnvironmentBuildFailed {
        #[source]
        source: SessionEnvironmentFactoryError,
    },
    /// 目标 Workspace 不存在。
    #[error("workspace `{workspace_id}` was not found")]
    WorkspaceNotFound { workspace_id: WorkspaceId },
    /// Workspace 已假删，不能绑定新 Session。
    #[error("workspace `{workspace_id}` was removed")]
    WorkspaceRemoved { workspace_id: WorkspaceId },
    /// Workspace 的用户目录当前不可访问。
    #[error("workspace `{workspace_id}` is unavailable")]
    WorkspaceUnavailable { workspace_id: WorkspaceId },
    /// 指定 Attachment 不存在于目标 Session。
    #[error("attachment `{attachment_id}` was not found in session `{session_id}`")]
    AttachmentNotFound {
        session_id: SessionId,
        attachment_id: AttachmentId,
    },
    /// 指定 Attachment 属于目标 Session，但正文或稳定视图当前不可用。
    #[error("attachment `{attachment_id}` is unavailable in session `{session_id}`")]
    AttachmentUnavailable {
        session_id: SessionId,
        attachment_id: AttachmentId,
    },
    #[error("approval `{approval_id}` was not found")]
    ApprovalNotFound { approval_id: ApprovalId },
    #[error("approval `{approval_id}` is no longer pending")]
    ApprovalExpired { approval_id: ApprovalId },
    #[error("approval `{approval_id}` is not at the head of its session queue")]
    ApprovalNotHead { approval_id: ApprovalId },
    #[error("queue revision does not match the current session queue")]
    QueueConflict,
    #[error("conversation cursor belongs to an older generation")]
    SnapshotStale,
    #[error("runtime state changed repeatedly while building a snapshot")]
    SnapshotBusy,
    #[error("the requested permission scope is unavailable")]
    PermissionScopeUnavailable,
    #[error("permission file is invalid")]
    PermissionFileInvalid,
    #[error("permission file changed during update")]
    PermissionFileConflict,
    #[error("permission rule could not be persisted")]
    PermissionPersistenceFailed,
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
    pub(crate) fn from_store(operation: &'static str, source: StoreError) -> Self {
        Self::StorageUnavailable {
            operation,
            source: Some(source),
        }
    }

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
            Self::SessionArchived { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::SessionArchived, "session is archived")
            }
            Self::SessionNotIdle { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::SessionNotIdle, "session is not idle")
            }
            Self::RunNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::RunNotFound, "run was not found")
            }
            Self::ChildTaskNotFound { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ChildTaskNotFound,
                "child task was not found",
            ),
            Self::InputNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::InputNotFound, "input was not found")
            }
            Self::RunNotRetryable { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::RunNotRetryable, "run is not retryable")
            }
            Self::SessionFaulted { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "session internal state is unavailable",
            ),
            Self::StorageUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::StorageUnavailable,
                "runtime storage is unavailable",
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
            Self::RunToolsBuildFailed { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::AgentBuildFailed,
                "tools could not be prepared for this run",
            ),
            Self::SessionEnvironmentBuildFailed { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::AgentBuildFailed,
                "session environment could not be created",
            ),
            Self::WorkspaceNotFound { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::WorkspaceNotFound,
                "workspace was not found",
            ),
            Self::WorkspaceRemoved { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::WorkspaceRemoved, "workspace was removed")
            }
            Self::WorkspaceUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::WorkspaceUnavailable,
                "workspace is unavailable",
            ),
            Self::AttachmentNotFound { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentNotFound,
                "attachment was not found",
            ),
            Self::AttachmentUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::AttachmentUnavailable,
                "attachment is unavailable",
            ),
            Self::ApprovalNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::ApprovalNotFound, "approval was not found")
            }
            Self::ApprovalExpired { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ApprovalExpired,
                "approval is no longer pending",
            ),
            Self::ApprovalNotHead { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ApprovalNotHead,
                "approval is not at the head of the queue",
            ),
            Self::QueueConflict => RuntimeErrorInfo::new(
                RuntimeErrorCode::QueueConflict,
                "queue changed before the operation was applied",
            ),
            Self::SnapshotStale => RuntimeErrorInfo::new(
                RuntimeErrorCode::SnapshotStale,
                "conversation snapshot changed; reload the latest page",
            ),
            Self::SnapshotBusy => RuntimeErrorInfo::new(
                RuntimeErrorCode::SnapshotBusy,
                "runtime state is changing; retry the snapshot",
            ),
            Self::PermissionScopeUnavailable => RuntimeErrorInfo::new(
                RuntimeErrorCode::PermissionScopeUnavailable,
                "permission scope is unavailable",
            ),
            Self::PermissionFileInvalid => RuntimeErrorInfo::new(
                RuntimeErrorCode::PermissionFileInvalid,
                "permission file is invalid",
            ),
            Self::PermissionFileConflict => RuntimeErrorInfo::new(
                RuntimeErrorCode::PermissionFileConflict,
                "permission file changed during update",
            ),
            Self::PermissionPersistenceFailed => RuntimeErrorInfo::new(
                RuntimeErrorCode::PermissionPersistenceFailed,
                "permission rule could not be persisted",
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
