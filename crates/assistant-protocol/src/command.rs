//! Runtime 客户端意图及其成功结果。

use serde::{Deserialize, Serialize};

use crate::{
    AttachmentId, AttachmentSummary, ConfigurationStatus, IdempotencyKey, InputId,
    ModelConfiguration, ModelKey, RunId, RunSnapshot, RuntimeLifecycle, SessionId,
    SessionListFilter, SessionSummary, WorkspaceId, WorkspaceSummary,
};

/// 查询当前配置总体状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetConfigStatusRequest {}

/// 当前配置总体状态查询结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetConfigStatusResult {
    /// 当前配置总体状态。
    pub status: ConfigurationStatus,
}

/// 按确定性顺序查询全部脱敏模型投影。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListModelsRequest {}

/// 全部模型的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListModelsResult {
    /// 按配置 key 确定性排序的模型投影。
    pub models: Vec<ModelConfiguration>,
}

/// 查询一个合法 model key 的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetModelRequest {
    /// 要查询的用户 model key。
    pub model_key: ModelKey,
}

/// 单个模型的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetModelResult {
    /// 指定模型的脱敏投影。
    pub model: ModelConfiguration,
}

/// 显式重新读取唯一配置源。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReloadConfigRequest {}

/// reload 后立即可见的配置总体状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReloadConfigResult {
    /// 本次 reload 原子交换出的配置总体状态。
    pub status: ConfigurationStatus,
}

/// 显式验证一个已配置模型的基本连接与协议响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidateModelConnectionRequest {
    /// 要验证的用户 model key。
    pub model_key: ModelKey,
}

/// 连接验证失败的稳定分类。
///
/// 这些值是应用层契约，不直接序列化 Provider SDK、HTTP 客户端或
/// `ModelError` 的内部类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionValidationFailureKind {
    /// 模型服务或固定验证请求无法从当前配置构造。
    Configuration,
    /// DNS、TLS、拒绝连接或建流后中断。
    Connection,
    /// 连接或整体请求超时。
    Timeout,
    /// Provider 拒绝当前 credential。
    Authentication,
    /// Provider 无法使用当前模型或固定最小请求。
    ModelUnavailable,
    /// Provider 限流。
    RateLimited,
    /// Provider 服务暂时不可用。
    ServiceUnavailable,
    /// Provider 以其他可识别状态拒绝请求。
    ProviderRejected,
    /// 响应编码、事件顺序或流终态不符合契约。
    Protocol,
}

/// 一次连接验证的脱敏失败。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConnectionValidationFailure {
    /// 客户端可稳定分支的失败分类。
    pub kind: ConnectionValidationFailureKind,
    /// 不包含 credential、prompt、Provider 原始正文或底层错误链的展示消息。
    pub message: String,
}

/// 连接验证的业务结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "failure", rename_all = "snake_case")]
pub enum ConnectionValidationOutcome {
    /// 模型流产生了唯一且合法的 `TurnFinished` 终态。
    Succeeded,
    /// 配置、传输、Provider 或协议验证失败。
    Failed(ConnectionValidationFailure),
}

/// 指定模型的连接验证结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidateModelConnectionResult {
    /// 本次使用的用户 model key。
    pub model_key: ModelKey,
    /// 成功或结构化失败。
    pub outcome: ConnectionValidationOutcome,
}

/// 登记或恢复一个本机 Workspace。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterWorkspaceRequest {
    /// 用户选择的本机绝对 UTF-8 目录路径。
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 查询一个 Workspace；已移除 Workspace 仍可查询。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetWorkspaceRequest {
    pub workspace_id: WorkspaceId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 按确定性顺序列出当前活动 Workspace。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListWorkspacesRequest {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListWorkspacesResult {
    pub workspaces: Vec<WorkspaceSummary>,
}

/// 从新 Session 的正常可选列表中假删一个 Workspace。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoveWorkspaceRequest {
    pub workspace_id: WorkspaceId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoveWorkspaceResult {
    pub workspace: WorkspaceSummary,
}

/// 查询 Session 中的一个 Attachment。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetAttachmentRequest {
    pub session_id: SessionId,
    pub attachment_id: AttachmentId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetAttachmentResult {
    pub attachment: AttachmentSummary,
}

/// 按创建顺序列出 Session 的全部 Attachment。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListAttachmentsRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListAttachmentsResult {
    pub attachments: Vec<AttachmentSummary>,
}

/// HTTP 流式上传完成后的稳定业务结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UploadAttachmentResult {
    pub attachment: AttachmentSummary,
}

/// 创建一个空 Session。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSessionRequest {
    /// 可选展示标题；`None` 表示由 Runtime 选择默认标题。
    pub title: Option<String>,
    /// 显式模型 key；`None` 表示使用创建时配置快照中的默认模型。
    pub model_key: Option<ModelKey>,
    /// 可选的 Workspace 冻结绑定；创建后不能直接换绑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// 创建 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSessionResult {
    /// 新创建的 Session 摘要。
    pub session: SessionSummary,
}

/// 按生命周期列出 Session；缺省只返回活动 Session。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListSessionsRequest {
    #[serde(default)]
    pub filter: SessionListFilter,
}

/// 列出 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListSessionsResult {
    /// 按 Runtime 确定性顺序返回的 Session 摘要。
    pub sessions: Vec<SessionSummary>,
}

/// 查询指定 Session。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetSessionRequest {
    /// 要查询的 Session。
    pub session_id: SessionId,
}

/// 查询 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetSessionResult {
    /// 当前 Session 摘要。
    pub session: SessionSummary,
}

/// 可靠提交一条用户输入；同 Session 内可按 key 幂等重试。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmitInputRequest {
    /// 目标 Session。
    pub session_id: SessionId,
    /// 原样进入规范 UserMessage 的文本；Runtime 负责非空白校验。
    pub message: String,
    /// 按用户选择顺序引用的 Session Attachment。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<AttachmentId>,
    /// 可选的不透明请求身份；重复 key 直接返回首次结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

/// 输入已持久化接受的结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmitInputResult {
    pub input_id: InputId,
    pub run: RunSnapshot,
}

/// 取消尚未进入 Conversation 的排队输入。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelQueuedInputRequest {
    pub session_id: SessionId,
    pub input_id: InputId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelQueuedInputResult {
    pub input_id: InputId,
}

/// 显式恢复重启后暂停的 Session 队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResumeSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResumeSessionResult {
    pub session: SessionSummary,
}

/// 为可重试的失败或中断 Run 创建新 attempt。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryRunRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetryRunResult {
    pub run: RunSnapshot,
}

/// 查询指定 Session 中的 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetRunRequest {
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 要查询的 Run。
    pub run_id: RunId,
}

/// 查询 Run 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetRunResult {
    /// 当前 Run 快照。
    pub run: RunSnapshot,
}

/// 查询指定 Session 的全部 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListRunsRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListRunsResult {
    pub runs: Vec<RunSnapshot>,
}

/// 把完全空闲的活动 Session 转为只读归档状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveSessionResult {
    pub session: SessionSummary,
}

/// 恢复一个归档 Session；不会自动启动任何 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RestoreSessionRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RestoreSessionResult {
    pub session: SessionSummary,
}

/// 切换 Session 后续 Run 使用的模型 key。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetSessionModelRequest {
    pub session_id: SessionId,
    pub model_key: ModelKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetSessionModelResult {
    pub session: SessionSummary,
}

/// 从历史 User Message 位置提交一条全新输入并销毁原目标及尾段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReenterFromUserMessageRequest {
    pub session_id: SessionId,
    pub message_id: crate::MessageId,
    pub message: String,
    /// 替换消息按用户选择顺序引用的 Session Attachment。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<AttachmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReenterFromUserMessageResult {
    pub input_id: InputId,
    pub run: RunSnapshot,
}

/// 请求取消指定 Session 中的活动 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelRunRequest {
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 要取消的 Run。
    pub run_id: RunId,
}

/// 取消请求被 Runtime 接受后的结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelRunResult {
    /// 已反映取消请求的当前 Run 快照。
    pub run: RunSnapshot,
}

/// 请求受控关闭 Runtime。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShutdownRuntimeRequest {}

/// 受控关闭请求被接受后的结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShutdownRuntimeResult {
    /// 接受请求后的 Runtime 生命周期。
    pub lifecycle: RuntimeLifecycle,
}

/// Runtime 支持的最小客户端意图。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RuntimeCommand {
    /// 查询配置总体状态。
    GetConfigStatus(GetConfigStatusRequest),
    /// 列出全部模型脱敏投影。
    ListModels(ListModelsRequest),
    /// 查询一个模型脱敏投影。
    GetModel(GetModelRequest),
    /// 显式重新加载配置。
    ReloadConfig(ReloadConfigRequest),
    /// 显式验证指定模型连接。
    ValidateModelConnection(ValidateModelConnectionRequest),
    /// 登记或恢复 Workspace。
    RegisterWorkspace(RegisterWorkspaceRequest),
    /// 查询 Workspace。
    GetWorkspace(GetWorkspaceRequest),
    /// 列出活动 Workspace。
    ListWorkspaces(ListWorkspacesRequest),
    /// 假删 Workspace。
    RemoveWorkspace(RemoveWorkspaceRequest),
    /// 查询 Attachment。
    GetAttachment(GetAttachmentRequest),
    /// 列出 Session Attachment。
    ListAttachments(ListAttachmentsRequest),
    /// 创建 Session。
    CreateSession(CreateSessionRequest),
    /// 列出 Session。
    ListSessions(ListSessionsRequest),
    /// 查询 Session。
    GetSession(GetSessionRequest),
    /// 提交持久化输入。
    SubmitInput(SubmitInputRequest),
    /// 取消排队输入。
    CancelQueuedInput(CancelQueuedInputRequest),
    /// 恢复重启后暂停的队列。
    ResumeSession(ResumeSessionRequest),
    /// 重试失败或中断 Run。
    RetryRun(RetryRunRequest),
    /// 查询 Run。
    GetRun(GetRunRequest),
    /// 列出 Session 的全部 Run。
    ListRuns(ListRunsRequest),
    /// 归档 Session。
    ArchiveSession(ArchiveSessionRequest),
    /// 恢复归档 Session。
    RestoreSession(RestoreSessionRequest),
    /// 切换 Session 模型。
    SetSessionModel(SetSessionModelRequest),
    /// 从历史 User Message 重新输入。
    ReenterFromUserMessage(ReenterFromUserMessageRequest),
    /// 取消 Run。
    CancelRun(CancelRunRequest),
    /// 关闭 Runtime。
    ShutdownRuntime(ShutdownRuntimeRequest),
}

/// Runtime 命令的成功结果；失败统一由 Host 发送 [`crate::RuntimeErrorInfo`]。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RuntimeCommandResult {
    /// 配置总体状态已返回。
    GetConfigStatus(GetConfigStatusResult),
    /// 模型列表已返回。
    ListModels(ListModelsResult),
    /// 单模型投影已返回。
    GetModel(GetModelResult),
    /// 配置已重新加载并返回新状态。
    ReloadConfig(ReloadConfigResult),
    /// 模型连接验证已完成。
    ValidateModelConnection(ValidateModelConnectionResult),
    /// Workspace 已登记或恢复。
    RegisterWorkspace(RegisterWorkspaceResult),
    /// Workspace 查询已返回。
    GetWorkspace(GetWorkspaceResult),
    /// Workspace 列表已返回。
    ListWorkspaces(ListWorkspacesResult),
    /// Workspace 已假删。
    RemoveWorkspace(RemoveWorkspaceResult),
    /// Attachment 查询已返回。
    GetAttachment(GetAttachmentResult),
    /// Attachment 列表已返回。
    ListAttachments(ListAttachmentsResult),
    /// Session 已创建。
    CreateSession(CreateSessionResult),
    /// Session 列表已返回。
    ListSessions(ListSessionsResult),
    /// Session 查询已返回。
    GetSession(GetSessionResult),
    /// 输入已接受。
    SubmitInput(SubmitInputResult),
    /// 排队输入已取消。
    CancelQueuedInput(CancelQueuedInputResult),
    /// Session 队列已恢复。
    ResumeSession(ResumeSessionResult),
    /// 新 Run attempt 已创建。
    RetryRun(RetryRunResult),
    /// Run 查询已返回。
    GetRun(GetRunResult),
    /// Run 列表已返回。
    ListRuns(ListRunsResult),
    /// Session 已归档。
    ArchiveSession(ArchiveSessionResult),
    /// Session 已恢复。
    RestoreSession(RestoreSessionResult),
    /// Session 模型已切换。
    SetSessionModel(SetSessionModelResult),
    /// 历史重新输入已接受。
    ReenterFromUserMessage(ReenterFromUserMessageResult),
    /// 取消请求已接受。
    CancelRun(CancelRunResult),
    /// Runtime 关闭请求已接受。
    ShutdownRuntime(ShutdownRuntimeResult),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session_summary() -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new("session-1").expect("session id"),
            title: "Session 1".to_owned(),
            model_key: ModelKey::new("model-1").expect("model key"),
            lifecycle: crate::SessionLifecycle::Active,
            workspace_id: None,
            active_run_id: None,
            message_count: 0,
            queued_input_count: 0,
            resume_required: false,
        }
    }

    fn run_snapshot() -> RunSnapshot {
        RunSnapshot {
            run_id: RunId::new("run-1").expect("run id"),
            session_id: SessionId::new("session-1").expect("session id"),
            input_id: InputId::new("input-1").expect("input id"),
            attempt: 1,
            status: crate::RunStatus::Accepted,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
        }
    }

    fn workspace_summary() -> WorkspaceSummary {
        WorkspaceSummary {
            workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
            user_directory: "/workspace".to_owned(),
            agent_directory: "/runtime/workspaces/workspace-1/agent".to_owned(),
            lifecycle: crate::WorkspaceLifecycle::Active,
            created_at_ms: 1,
            updated_at_ms: 1,
            removed_at_ms: None,
        }
    }

    fn configuration_status() -> ConfigurationStatus {
        ConfigurationStatus {
            config_path: Some("/private/runtime/config.toml".to_owned()),
            state: crate::ConfigurationState::Ready,
            schema_version: Some(1),
            default_model: Some(ModelKey::new("model-1").expect("model key")),
            issues: Vec::new(),
        }
    }

    fn model_configuration() -> ModelConfiguration {
        ModelConfiguration {
            model_key: Some(ModelKey::new("model-1").expect("model key")),
            display_name: "Model 1".to_owned(),
            protocol: Some("chat_completions".to_owned()),
            provider: Some("fixture".to_owned()),
            endpoint: Some("https://api.example.test/v1".to_owned()),
            model: Some("fixture-model".to_owned()),
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(4_096),
            agent_max_output_tokens: None,
            effective_max_output_tokens: Some(4_096),
            api_key_configured: true,
            is_default: true,
            is_valid: true,
            issues: Vec::new(),
        }
    }

    #[test]
    fn command_uses_explicit_type_and_payload_tags() {
        let command = RuntimeCommand::SubmitInput(SubmitInputRequest {
            session_id: SessionId::new("session-1").expect("session id"),
            message: "hello".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        });
        let value = serde_json::to_value(&command).expect("serialize command");

        assert_eq!(
            value,
            json!({
                "type": "submit_input",
                "payload": {
                    "session_id": "session-1",
                    "message": "hello"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeCommand>(value).expect("deserialize command"),
            command
        );
    }

    #[test]
    fn input_attachment_ids_are_ordered_and_default_to_empty() {
        let legacy = serde_json::from_value::<SubmitInputRequest>(json!({
            "session_id": "session-1",
            "message": "hello"
        }))
        .expect("legacy input request");
        assert!(legacy.attachment_ids.is_empty());

        let request = SubmitInputRequest {
            session_id: SessionId::new("session-1").expect("session id"),
            message: "compare".to_owned(),
            attachment_ids: vec![
                AttachmentId::new("attachment-2").expect("attachment id"),
                AttachmentId::new("attachment-1").expect("attachment id"),
            ],
            idempotency_key: None,
        };
        assert_eq!(
            serde_json::to_value(request).expect("serialize input request")["attachment_ids"],
            json!(["attachment-2", "attachment-1"])
        );
    }

    #[test]
    fn command_result_uses_matching_explicit_tag() {
        let result = RuntimeCommandResult::ShutdownRuntime(ShutdownRuntimeResult {
            lifecycle: RuntimeLifecycle::ShuttingDown,
        });
        let value = serde_json::to_value(&result).expect("serialize result");

        assert_eq!(
            value,
            json!({
                "type": "shutdown_runtime",
                "payload": { "lifecycle": "shutting_down" }
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeCommandResult>(value).expect("deserialize result"),
            result
        );
    }

    #[test]
    fn every_command_and_result_variant_round_trips() {
        let session_id = SessionId::new("session-1").expect("session id");
        let run_id = RunId::new("run-1").expect("run id");
        let commands = vec![
            (
                RuntimeCommand::GetConfigStatus(GetConfigStatusRequest::default()),
                "get_config_status",
            ),
            (
                RuntimeCommand::ListModels(ListModelsRequest::default()),
                "list_models",
            ),
            (
                RuntimeCommand::GetModel(GetModelRequest {
                    model_key: ModelKey::new("model-1").expect("model key"),
                }),
                "get_model",
            ),
            (
                RuntimeCommand::ReloadConfig(ReloadConfigRequest::default()),
                "reload_config",
            ),
            (
                RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest {
                    model_key: ModelKey::new("model-1").expect("model key"),
                }),
                "validate_model_connection",
            ),
            (
                RuntimeCommand::RegisterWorkspace(RegisterWorkspaceRequest {
                    path: "/workspace".to_owned(),
                }),
                "register_workspace",
            ),
            (
                RuntimeCommand::GetWorkspace(GetWorkspaceRequest {
                    workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
                }),
                "get_workspace",
            ),
            (
                RuntimeCommand::ListWorkspaces(ListWorkspacesRequest::default()),
                "list_workspaces",
            ),
            (
                RuntimeCommand::RemoveWorkspace(RemoveWorkspaceRequest {
                    workspace_id: WorkspaceId::new("workspace-1").expect("workspace id"),
                }),
                "remove_workspace",
            ),
            (
                RuntimeCommand::CreateSession(CreateSessionRequest::default()),
                "create_session",
            ),
            (
                RuntimeCommand::ListSessions(ListSessionsRequest::default()),
                "list_sessions",
            ),
            (
                RuntimeCommand::GetSession(GetSessionRequest {
                    session_id: session_id.clone(),
                }),
                "get_session",
            ),
            (
                RuntimeCommand::SubmitInput(SubmitInputRequest {
                    session_id: session_id.clone(),
                    message: "hello".to_owned(),
                    attachment_ids: Vec::new(),
                    idempotency_key: None,
                }),
                "submit_input",
            ),
            (
                RuntimeCommand::CancelQueuedInput(CancelQueuedInputRequest {
                    session_id: session_id.clone(),
                    input_id: InputId::new("input-1").expect("input id"),
                }),
                "cancel_queued_input",
            ),
            (
                RuntimeCommand::ResumeSession(ResumeSessionRequest {
                    session_id: session_id.clone(),
                }),
                "resume_session",
            ),
            (
                RuntimeCommand::RetryRun(RetryRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                }),
                "retry_run",
            ),
            (
                RuntimeCommand::GetRun(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                }),
                "get_run",
            ),
            (
                RuntimeCommand::ListRuns(ListRunsRequest {
                    session_id: session_id.clone(),
                }),
                "list_runs",
            ),
            (
                RuntimeCommand::ArchiveSession(ArchiveSessionRequest {
                    session_id: session_id.clone(),
                }),
                "archive_session",
            ),
            (
                RuntimeCommand::RestoreSession(RestoreSessionRequest {
                    session_id: session_id.clone(),
                }),
                "restore_session",
            ),
            (
                RuntimeCommand::SetSessionModel(SetSessionModelRequest {
                    session_id: session_id.clone(),
                    model_key: ModelKey::new("model-2").expect("model key"),
                }),
                "set_session_model",
            ),
            (
                RuntimeCommand::ReenterFromUserMessage(ReenterFromUserMessageRequest {
                    session_id: session_id.clone(),
                    message_id: crate::MessageId::new("message-1").expect("message id"),
                    message: "replacement".to_owned(),
                    attachment_ids: Vec::new(),
                    idempotency_key: Some(
                        IdempotencyKey::new("replace-1").expect("idempotency key"),
                    ),
                }),
                "reenter_from_user_message",
            ),
            (
                RuntimeCommand::CancelRun(CancelRunRequest { session_id, run_id }),
                "cancel_run",
            ),
            (
                RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
                "shutdown_runtime",
            ),
        ];
        for (command, tag) in commands {
            let value = serde_json::to_value(&command).expect("serialize command");
            assert_eq!(value["type"], tag);
            assert_eq!(
                serde_json::from_value::<RuntimeCommand>(value).expect("deserialize command"),
                command
            );
        }

        let results = vec![
            (
                RuntimeCommandResult::GetConfigStatus(GetConfigStatusResult {
                    status: configuration_status(),
                }),
                "get_config_status",
            ),
            (
                RuntimeCommandResult::ListModels(ListModelsResult {
                    models: vec![model_configuration()],
                }),
                "list_models",
            ),
            (
                RuntimeCommandResult::GetModel(GetModelResult {
                    model: model_configuration(),
                }),
                "get_model",
            ),
            (
                RuntimeCommandResult::ReloadConfig(ReloadConfigResult {
                    status: configuration_status(),
                }),
                "reload_config",
            ),
            (
                RuntimeCommandResult::ValidateModelConnection(ValidateModelConnectionResult {
                    model_key: ModelKey::new("model-1").expect("model key"),
                    outcome: ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
                        kind: ConnectionValidationFailureKind::Authentication,
                        message: "model authentication failed".to_owned(),
                    }),
                }),
                "validate_model_connection",
            ),
            (
                RuntimeCommandResult::RegisterWorkspace(RegisterWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "register_workspace",
            ),
            (
                RuntimeCommandResult::GetWorkspace(GetWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "get_workspace",
            ),
            (
                RuntimeCommandResult::ListWorkspaces(ListWorkspacesResult {
                    workspaces: vec![workspace_summary()],
                }),
                "list_workspaces",
            ),
            (
                RuntimeCommandResult::RemoveWorkspace(RemoveWorkspaceResult {
                    workspace: workspace_summary(),
                }),
                "remove_workspace",
            ),
            (
                RuntimeCommandResult::CreateSession(CreateSessionResult {
                    session: session_summary(),
                }),
                "create_session",
            ),
            (
                RuntimeCommandResult::ListSessions(ListSessionsResult {
                    sessions: vec![session_summary()],
                }),
                "list_sessions",
            ),
            (
                RuntimeCommandResult::GetSession(GetSessionResult {
                    session: session_summary(),
                }),
                "get_session",
            ),
            (
                RuntimeCommandResult::SubmitInput(SubmitInputResult {
                    input_id: InputId::new("input-1").expect("input id"),
                    run: run_snapshot(),
                }),
                "submit_input",
            ),
            (
                RuntimeCommandResult::CancelQueuedInput(CancelQueuedInputResult {
                    input_id: InputId::new("input-1").expect("input id"),
                }),
                "cancel_queued_input",
            ),
            (
                RuntimeCommandResult::ResumeSession(ResumeSessionResult {
                    session: session_summary(),
                }),
                "resume_session",
            ),
            (
                RuntimeCommandResult::RetryRun(RetryRunResult {
                    run: run_snapshot(),
                }),
                "retry_run",
            ),
            (
                RuntimeCommandResult::GetRun(GetRunResult {
                    run: run_snapshot(),
                }),
                "get_run",
            ),
            (
                RuntimeCommandResult::ListRuns(ListRunsResult {
                    runs: vec![run_snapshot()],
                }),
                "list_runs",
            ),
            (
                RuntimeCommandResult::ArchiveSession(ArchiveSessionResult {
                    session: session_summary(),
                }),
                "archive_session",
            ),
            (
                RuntimeCommandResult::RestoreSession(RestoreSessionResult {
                    session: session_summary(),
                }),
                "restore_session",
            ),
            (
                RuntimeCommandResult::SetSessionModel(SetSessionModelResult {
                    session: session_summary(),
                }),
                "set_session_model",
            ),
            (
                RuntimeCommandResult::ReenterFromUserMessage(ReenterFromUserMessageResult {
                    input_id: InputId::new("input-1").expect("input id"),
                    run: run_snapshot(),
                }),
                "reenter_from_user_message",
            ),
            (
                RuntimeCommandResult::CancelRun(CancelRunResult {
                    run: run_snapshot(),
                }),
                "cancel_run",
            ),
            (
                RuntimeCommandResult::ShutdownRuntime(ShutdownRuntimeResult {
                    lifecycle: RuntimeLifecycle::ShuttingDown,
                }),
                "shutdown_runtime",
            ),
        ];
        for (result, tag) in results {
            let value = serde_json::to_value(&result).expect("serialize result");
            assert_eq!(value["type"], tag);
            assert_eq!(
                serde_json::from_value::<RuntimeCommandResult>(value).expect("deserialize result"),
                result
            );
        }
    }

    #[test]
    fn every_connection_failure_kind_has_a_stable_wire_value() {
        let cases = [
            (
                ConnectionValidationFailureKind::Configuration,
                "configuration",
            ),
            (ConnectionValidationFailureKind::Connection, "connection"),
            (ConnectionValidationFailureKind::Timeout, "timeout"),
            (
                ConnectionValidationFailureKind::Authentication,
                "authentication",
            ),
            (
                ConnectionValidationFailureKind::ModelUnavailable,
                "model_unavailable",
            ),
            (ConnectionValidationFailureKind::RateLimited, "rate_limited"),
            (
                ConnectionValidationFailureKind::ServiceUnavailable,
                "service_unavailable",
            ),
            (
                ConnectionValidationFailureKind::ProviderRejected,
                "provider_rejected",
            ),
            (ConnectionValidationFailureKind::Protocol, "protocol"),
        ];

        for (kind, wire) in cases {
            assert_eq!(
                serde_json::to_string(&kind).expect("serialize validation failure kind"),
                format!("\"{wire}\"")
            );
        }
    }
}
