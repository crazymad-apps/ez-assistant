//! 可安全跨进程传输的 Runtime 错误。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 客户端可以稳定分支处理的 Runtime 错误码。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeErrorCode {
    /// 命令字段缺失、格式错误或业务输入无效。
    InvalidRequest,
    /// 指定 Session 不存在。
    SessionNotFound,
    /// 指定 Session 已有活动 Run 或未完成结算。
    SessionBusy,
    /// Session 已归档，只允许查询。
    SessionArchived,
    /// Session 尚未达到归档、模型切换或历史重新输入要求的完全空闲状态。
    SessionNotIdle,
    /// Session 已有自动或手动上下文压缩正在执行。
    SessionCompactionInProgress,
    /// 指定的手动压缩已经结束或不存在。
    SessionCompactionNotFound,
    /// 当前没有可用的主控会话。
    ControllerUnavailable,
    /// Session 的持久角色不允许当前生命周期操作。
    SessionRoleRestricted,
    /// 指定 Run 不存在。
    RunNotFound,
    /// 指定子任务不存在于目标 Session。
    ChildTaskNotFound,
    /// 指定持久化输入不存在。
    InputNotFound,
    /// 指定 Run 的状态不允许创建新的执行尝试。
    RunNotRetryable,
    /// Session 已存在尚未清除的 Goal。
    GoalAlreadyExists,
    /// 指定 Session 没有可控制的 Goal。
    GoalNotFound,
    /// 客户端携带的 Goal ID 或 generation 已经过期。
    GoalGenerationConflict,
    /// Goal 当前状态不允许恢复。
    GoalNotResumable,
    /// Goal-bound Run 不能使用普通 Retry/Cancel，必须显式恢复或停止 Goal。
    GoalRunRequiresResume,
    /// 当前 Session 模型不支持 Goal 所需的 Tool Call。
    GoalUnsupportedByModel,
    /// 用户提交的 Skill 名称格式无效。
    SkillNameInvalid,
    /// 当前 Session 没有可用于激活的冻结 Skill Catalog。
    SkillCatalogUnavailable,
    /// 当前 Session Catalog 中不存在指定 Skill。
    SkillNotFound,
    /// 指定 Skill 不允许用户显式激活。
    SkillNotUserInvocable,
    /// WorkPlan revision 已变化。
    WorkPlanRevisionConflict,
    /// Runtime 的权威存储当前不可用或拒绝了业务提交。
    StorageUnavailable,
    /// Runtime 已开始关闭，不再接受新的变更操作。
    RuntimeShuttingDown,
    /// 为一次 Run 构造 Agent 失败。
    AgentBuildFailed,
    /// 当前配置无法提供模型快照。
    ConfigurationUnavailable,
    /// 指定模型 key 不存在。
    ModelNotFound,
    /// 指定模型条目存在但当前无效。
    ModelUnavailable,
    /// 已校验模型无法构造为本次 Run 的服务或 Agent。
    ModelBuildFailed,
    /// 模型调用已经开始，但本次 Provider Turn 最终失败。
    ModelExecutionFailed,
    /// Runtime 无法完成一次自动上下文压缩或达到压缩恢复上限。
    ContextCompactionFailed,
    /// 一项受 Runtime 限时控制的操作已经超时。
    Timeout,
    /// 一项可独立取消的操作已经完成取消收敛。
    Cancelled,
    /// 指定 Workspace 不存在。
    WorkspaceNotFound,
    /// 指定 Workspace 已移除，不能绑定新 Session。
    WorkspaceRemoved,
    /// Workspace 用户目录当前不可访问。
    WorkspaceUnavailable,
    /// 指定 Attachment 不存在于目标 Session。
    AttachmentNotFound,
    /// Attachment 正文或稳定视图当前不可用。
    AttachmentUnavailable,
    /// 上传文件超过 Host 公布的单文件上限。
    AttachmentTooLarge,
    /// 上传请求、文件名或 multipart 结构无效。
    AttachmentUploadInvalid,
    /// 权限文件内容无法被当前 schema 完整接受。
    PermissionFileInvalid,
    /// 权限文件在修改期间发生 revision 冲突。
    PermissionFileConflict,
    /// 显式权限重载无法完成。
    PermissionReloadFailed,
    /// Runtime 无法持久化一条权限规则。
    PermissionPersistenceFailed,
    ApprovalNotFound,
    ApprovalExpired,
    /// 审批仍存在，但不是当前 Session 队列中允许决策的队首。
    ApprovalNotHead,
    /// 审批已经被其他客户端或并发操作完成。
    ApprovalAlreadyResolved,
    PermissionScopeUnavailable,
    /// 一般业务状态或 expected revision 已变化。
    Conflict,
    /// 模型配置 revision 已变化。
    ConfigurationConflict,
    /// 输入队列 revision 已变化。
    QueueConflict,
    /// Conversation generation 或快照依赖已经变化。
    SnapshotStale,
    /// clear 已切换到权威空历史，但物理清理尚待恢复收敛。
    SessionHistoryCleanupPending,
    /// 组合快照在有界重试内无法取得同一观察水位。
    SnapshotBusy,
    /// 当前资源状态不允许执行请求的操作。
    OperationNotAllowed,
    /// 资源存在，但不允许形成预览。
    ResourceNotPreviewable,
    /// 资源超过产品查询或预览上限。
    ResourceTooLarge,
    /// 不应向客户端暴露内部细节的故障。
    Internal,
}

/// 模型 attempt 失败的脱敏稳定分类。
///
/// 该分类只表达可安全展示和聚合的故障事实，不携带 Provider 原始错误正文、
/// prompt、credential 或请求内容。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ModelFailureKind {
    Configuration,
    Authentication,
    Connection,
    Timeout,
    StreamInterrupted,
    ProviderRejected,
    RateLimited,
    ServiceUnavailable,
    ContextOverflow,
    Protocol,
    ToolArguments,
    Resource,
    Cancelled,
}

/// Host 可以发送给客户端的脱敏错误信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeErrorInfo {
    /// 稳定、可供客户端判断的错误码。
    pub code: RuntimeErrorCode,
    /// 不包含 credential、prompt、Provider 正文或 panic payload 的安全消息。
    pub message: String,
}

impl RuntimeErrorInfo {
    /// 构造一条已经在 Runtime/Host 边界完成脱敏的错误信息。
    pub fn new(code: RuntimeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_has_a_stable_snake_case_wire_value() {
        let cases = [
            (RuntimeErrorCode::InvalidRequest, "invalid_request"),
            (RuntimeErrorCode::SessionNotFound, "session_not_found"),
            (RuntimeErrorCode::SessionBusy, "session_busy"),
            (RuntimeErrorCode::SessionArchived, "session_archived"),
            (RuntimeErrorCode::SessionNotIdle, "session_not_idle"),
            (
                RuntimeErrorCode::SessionCompactionInProgress,
                "session_compaction_in_progress",
            ),
            (
                RuntimeErrorCode::SessionCompactionNotFound,
                "session_compaction_not_found",
            ),
            (RuntimeErrorCode::RunNotFound, "run_not_found"),
            (RuntimeErrorCode::ChildTaskNotFound, "child_task_not_found"),
            (RuntimeErrorCode::InputNotFound, "input_not_found"),
            (RuntimeErrorCode::RunNotRetryable, "run_not_retryable"),
            (RuntimeErrorCode::GoalAlreadyExists, "goal_already_exists"),
            (RuntimeErrorCode::GoalNotFound, "goal_not_found"),
            (
                RuntimeErrorCode::GoalGenerationConflict,
                "goal_generation_conflict",
            ),
            (RuntimeErrorCode::GoalNotResumable, "goal_not_resumable"),
            (
                RuntimeErrorCode::GoalRunRequiresResume,
                "goal_run_requires_resume",
            ),
            (
                RuntimeErrorCode::GoalUnsupportedByModel,
                "goal_unsupported_by_model",
            ),
            (RuntimeErrorCode::SkillNameInvalid, "skill_name_invalid"),
            (
                RuntimeErrorCode::SkillCatalogUnavailable,
                "skill_catalog_unavailable",
            ),
            (RuntimeErrorCode::SkillNotFound, "skill_not_found"),
            (
                RuntimeErrorCode::SkillNotUserInvocable,
                "skill_not_user_invocable",
            ),
            (
                RuntimeErrorCode::WorkPlanRevisionConflict,
                "work_plan_revision_conflict",
            ),
            (RuntimeErrorCode::StorageUnavailable, "storage_unavailable"),
            (
                RuntimeErrorCode::RuntimeShuttingDown,
                "runtime_shutting_down",
            ),
            (RuntimeErrorCode::AgentBuildFailed, "agent_build_failed"),
            (
                RuntimeErrorCode::ConfigurationUnavailable,
                "configuration_unavailable",
            ),
            (RuntimeErrorCode::ModelNotFound, "model_not_found"),
            (RuntimeErrorCode::ModelUnavailable, "model_unavailable"),
            (RuntimeErrorCode::ModelBuildFailed, "model_build_failed"),
            (
                RuntimeErrorCode::ModelExecutionFailed,
                "model_execution_failed",
            ),
            (
                RuntimeErrorCode::ContextCompactionFailed,
                "context_compaction_failed",
            ),
            (RuntimeErrorCode::Timeout, "timeout"),
            (RuntimeErrorCode::Cancelled, "cancelled"),
            (RuntimeErrorCode::WorkspaceNotFound, "workspace_not_found"),
            (RuntimeErrorCode::WorkspaceRemoved, "workspace_removed"),
            (
                RuntimeErrorCode::WorkspaceUnavailable,
                "workspace_unavailable",
            ),
            (RuntimeErrorCode::AttachmentNotFound, "attachment_not_found"),
            (
                RuntimeErrorCode::AttachmentUnavailable,
                "attachment_unavailable",
            ),
            (RuntimeErrorCode::AttachmentTooLarge, "attachment_too_large"),
            (
                RuntimeErrorCode::AttachmentUploadInvalid,
                "attachment_upload_invalid",
            ),
            (
                RuntimeErrorCode::PermissionFileInvalid,
                "permission_file_invalid",
            ),
            (
                RuntimeErrorCode::PermissionFileConflict,
                "permission_file_conflict",
            ),
            (
                RuntimeErrorCode::PermissionReloadFailed,
                "permission_reload_failed",
            ),
            (
                RuntimeErrorCode::PermissionPersistenceFailed,
                "permission_persistence_failed",
            ),
            (RuntimeErrorCode::ApprovalNotFound, "approval_not_found"),
            (RuntimeErrorCode::ApprovalExpired, "approval_expired"),
            (RuntimeErrorCode::ApprovalNotHead, "approval_not_head"),
            (
                RuntimeErrorCode::ApprovalAlreadyResolved,
                "approval_already_resolved",
            ),
            (
                RuntimeErrorCode::PermissionScopeUnavailable,
                "permission_scope_unavailable",
            ),
            (RuntimeErrorCode::Conflict, "conflict"),
            (
                RuntimeErrorCode::ConfigurationConflict,
                "configuration_conflict",
            ),
            (RuntimeErrorCode::QueueConflict, "queue_conflict"),
            (RuntimeErrorCode::SnapshotStale, "snapshot_stale"),
            (
                RuntimeErrorCode::SessionHistoryCleanupPending,
                "session_history_cleanup_pending",
            ),
            (RuntimeErrorCode::SnapshotBusy, "snapshot_busy"),
            (
                RuntimeErrorCode::OperationNotAllowed,
                "operation_not_allowed",
            ),
            (
                RuntimeErrorCode::ResourceNotPreviewable,
                "resource_not_previewable",
            ),
            (RuntimeErrorCode::ResourceTooLarge, "resource_too_large"),
            (RuntimeErrorCode::Internal, "internal"),
        ];

        for (code, wire) in cases {
            assert_eq!(
                serde_json::to_string(&code).expect("serialize error code"),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<RuntimeErrorCode>(&format!("\"{wire}\""))
                    .expect("deserialize error code"),
                code
            );
        }
    }

    #[test]
    fn safe_error_info_round_trips() {
        let error = RuntimeErrorInfo::new(RuntimeErrorCode::SessionBusy, "session is busy");
        let json = serde_json::to_string(&error).expect("serialize error");
        assert_eq!(
            serde_json::from_str::<RuntimeErrorInfo>(&json).expect("deserialize error"),
            error
        );
    }
}
