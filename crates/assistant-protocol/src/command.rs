//! Runtime 客户端意图及其成功结果。

use serde::{Deserialize, Serialize};

use crate::{
    ConfigurationStatus, ModelConfiguration, ModelKey, RunId, RunSnapshot, RuntimeLifecycle,
    SessionId, SessionSummary,
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

/// 创建一个空 Session。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSessionRequest {
    /// 可选展示标题；`None` 表示由 Runtime 选择默认标题。
    pub title: Option<String>,
    /// 显式模型 key；`None` 表示使用创建时配置快照中的默认模型。
    pub model_key: Option<ModelKey>,
}

/// 创建 Session 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSessionResult {
    /// 新创建的 Session 摘要。
    pub session: SessionSummary,
}

/// 列出当前进程内所有 Session。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListSessionsRequest {}

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

/// 向一个空闲 Session 提交用户消息并启动 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartRunRequest {
    /// 目标 Session。
    pub session_id: SessionId,
    /// 原样进入规范 UserMessage 的文本；Runtime 负责非空白校验。
    pub message: String,
}

/// 启动 Run 的成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartRunResult {
    /// 已被 Runtime 接受的 Run 快照。
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
    /// 创建 Session。
    CreateSession(CreateSessionRequest),
    /// 列出 Session。
    ListSessions(ListSessionsRequest),
    /// 查询 Session。
    GetSession(GetSessionRequest),
    /// 启动 Run。
    StartRun(StartRunRequest),
    /// 查询 Run。
    GetRun(GetRunRequest),
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
    /// Session 已创建。
    CreateSession(CreateSessionResult),
    /// Session 列表已返回。
    ListSessions(ListSessionsResult),
    /// Session 查询已返回。
    GetSession(GetSessionResult),
    /// Run 已接受。
    StartRun(StartRunResult),
    /// Run 查询已返回。
    GetRun(GetRunResult),
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
            active_run_id: None,
            message_count: 0,
        }
    }

    fn run_snapshot() -> RunSnapshot {
        RunSnapshot {
            run_id: RunId::new("run-1").expect("run id"),
            session_id: SessionId::new("session-1").expect("session id"),
            status: crate::RunStatus::Accepted,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
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
        let command = RuntimeCommand::StartRun(StartRunRequest {
            session_id: SessionId::new("session-1").expect("session id"),
            message: "hello".to_owned(),
        });
        let value = serde_json::to_value(&command).expect("serialize command");

        assert_eq!(
            value,
            json!({
                "type": "start_run",
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
                RuntimeCommand::StartRun(StartRunRequest {
                    session_id: session_id.clone(),
                    message: "hello".to_owned(),
                }),
                "start_run",
            ),
            (
                RuntimeCommand::GetRun(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                }),
                "get_run",
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
                RuntimeCommandResult::StartRun(StartRunResult {
                    run: run_snapshot(),
                }),
                "start_run",
            ),
            (
                RuntimeCommandResult::GetRun(GetRunResult {
                    run: run_snapshot(),
                }),
                "get_run",
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
