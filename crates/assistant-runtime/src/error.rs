//! Runtime library 的结构化错误及应用协议映射。

use agent_model::ModelError;
use agent_sdk::AgentBuildError;
use assistant_protocol::{
    ApprovalId, AttachmentId, ChildTaskId, GoalId, InputId, ModelKey, RunId, RuntimeErrorCode,
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
    /// Session 已有自动或手动上下文压缩正在执行。
    #[error("session `{session_id}` context compaction is already in progress")]
    SessionCompactionInProgress { session_id: SessionId },
    /// 取消目标不是当前仍可取消的手动压缩。
    #[error("session `{session_id}` context compaction was not found")]
    SessionCompactionNotFound { session_id: SessionId },
    /// 当前没有可供产品使用的主控会话。
    #[error("controller session is unavailable")]
    ControllerUnavailable,
    /// Controller 不允许归档、删除或 Fork。
    #[error("session `{session_id}` role does not allow this operation")]
    SessionRoleRestricted { session_id: SessionId },
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
    /// Session 已存在尚未清除的 Goal。
    #[error("session `{session_id}` already has a Goal")]
    GoalAlreadyExists { session_id: SessionId },
    /// 目标 Session 没有未清除的 Goal 控制器。
    #[error("session `{session_id}` has no Goal")]
    GoalNotFound { session_id: SessionId },
    /// 客户端基于旧 GoalId 或 generation 发起了控制操作。
    #[error("Goal `{goal_id}` generation changed in session `{session_id}")]
    GoalGenerationConflict {
        session_id: SessionId,
        goal_id: GoalId,
    },
    /// Goal 存在，但当前状态不允许恢复。
    #[error("Goal `{goal_id}` is not resumable in session `{session_id}")]
    GoalNotResumable {
        session_id: SessionId,
        goal_id: GoalId,
    },
    /// Goal-bound Run 必须通过 Goal 生命周期命令继续处理。
    #[error("run `{run_id}` in session `{session_id}` requires Goal resume")]
    GoalRunRequiresResume {
        session_id: SessionId,
        run_id: RunId,
    },
    /// 当前 Session 模型不具备 Goal 所需的 Tool Call 能力。
    #[error("session `{session_id}` model does not support Goal execution")]
    GoalUnsupportedByModel { session_id: SessionId },
    /// 用户提交的 Skill 名称格式无效。
    #[error("skill name is invalid")]
    SkillNameInvalid,
    /// 当前 Session 没有可用于激活的冻结 Skill Catalog。
    #[error("session `{session_id}` skill catalog is unavailable")]
    SkillCatalogUnavailable { session_id: SessionId },
    /// 当前 Session Catalog 中不存在指定 Skill。
    #[error("skill was not found in session `{session_id}`")]
    SkillNotFound { session_id: SessionId },
    /// 指定 Skill 不允许用户显式激活。
    #[error("skill is not user invocable in session `{session_id}`")]
    SkillNotUserInvocable { session_id: SessionId },
    /// WorkPlan revision 已被其他写入更新。
    #[error("work plan revision changed in session `{session_id}")]
    WorkPlanRevisionConflict { session_id: SessionId },
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
    /// 配置文件 revision 已被另一个写入者更新。
    #[error("runtime configuration changed before the operation was applied")]
    ConfigurationConflict,
    /// Runtime 无法持久化已通过编译的配置 candidate。
    #[error("runtime configuration could not be persisted")]
    ConfigurationPersistenceFailed,
    /// 独立命令中的模型调用已经开始，但 Provider Turn 最终失败。
    #[error("model execution failed")]
    ModelExecutionFailed {
        /// 保留在 Runtime 内部的原始模型错误；协议层只返回脱敏分类。
        #[source]
        source: ModelError,
    },
    /// 独立手动上下文压缩未能生成或提交受控结果。
    #[error("session context compaction failed")]
    ContextCompactionFailed {
        /// 保留压缩布局、响应或替换校验的完整内部错误链。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
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
            Self::SessionCompactionInProgress { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SessionCompactionInProgress,
                "session context compaction is already in progress",
            ),
            Self::SessionCompactionNotFound { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SessionCompactionNotFound,
                "session context compaction was not found",
            ),
            Self::ControllerUnavailable => RuntimeErrorInfo::new(
                RuntimeErrorCode::ControllerUnavailable,
                "controller session is unavailable",
            ),
            Self::SessionRoleRestricted { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SessionRoleRestricted,
                "session role does not allow this operation",
            ),
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
            Self::GoalAlreadyExists { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::GoalAlreadyExists,
                "session already has a Goal",
            ),
            Self::GoalNotFound { .. } => {
                RuntimeErrorInfo::new(RuntimeErrorCode::GoalNotFound, "Goal was not found")
            }
            Self::GoalGenerationConflict { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::GoalGenerationConflict,
                "Goal changed; reload the latest state",
            ),
            Self::GoalNotResumable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::GoalNotResumable,
                "Goal cannot be resumed from its current state",
            ),
            Self::GoalRunRequiresResume { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::GoalRunRequiresResume,
                "Goal run must be handled through Goal controls",
            ),
            Self::GoalUnsupportedByModel { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::GoalUnsupportedByModel,
                "the current model does not support Goal execution",
            ),
            Self::SkillNameInvalid => {
                RuntimeErrorInfo::new(RuntimeErrorCode::SkillNameInvalid, "skill name is invalid")
            }
            Self::SkillCatalogUnavailable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SkillCatalogUnavailable,
                "the session skill catalog is unavailable",
            ),
            Self::SkillNotFound { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SkillNotFound,
                "skill was not found in the session catalog",
            ),
            Self::SkillNotUserInvocable { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::SkillNotUserInvocable,
                "skill is not available for user activation",
            ),
            Self::WorkPlanRevisionConflict { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::WorkPlanRevisionConflict,
                "work plan changed; reload the latest plan",
            ),
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
            Self::ConfigurationConflict => RuntimeErrorInfo::new(
                RuntimeErrorCode::ConfigurationConflict,
                "configuration changed; reload and review the latest values",
            ),
            Self::ConfigurationPersistenceFailed => RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "configuration could not be persisted",
            ),
            Self::ModelExecutionFailed { source } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ModelExecutionFailed,
                format!(
                    "model execution failed (kind={})",
                    crate::run::model_failure_kind_value(crate::run::model_failure_kind(source))
                ),
            ),
            Self::ContextCompactionFailed { .. } => RuntimeErrorInfo::new(
                RuntimeErrorCode::ContextCompactionFailed,
                "session context compaction failed",
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
