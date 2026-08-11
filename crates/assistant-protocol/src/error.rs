//! 可安全跨进程传输的 Runtime 错误。

use serde::{Deserialize, Serialize};

/// 客户端可以稳定分支处理的 Runtime 错误码。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    /// 指定 Run 不存在。
    RunNotFound,
    /// 指定持久化输入不存在。
    InputNotFound,
    /// 指定 Run 的状态不允许创建新的执行尝试。
    RunNotRetryable,
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
    /// 不应向客户端暴露内部细节的故障。
    Internal,
}

/// 模型 attempt 失败的脱敏稳定分类。
///
/// 该分类只表达可安全展示和聚合的故障事实，不携带 Provider 原始错误正文、
/// prompt、credential 或请求内容。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    Cancelled,
}

/// Host 可以发送给客户端的脱敏错误信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
            (RuntimeErrorCode::RunNotFound, "run_not_found"),
            (RuntimeErrorCode::InputNotFound, "input_not_found"),
            (RuntimeErrorCode::RunNotRetryable, "run_not_retryable"),
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
