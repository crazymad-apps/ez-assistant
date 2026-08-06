//! Runtime 客户端意图及其成功结果。

use serde::{Deserialize, Serialize};

use crate::{RunId, RunSnapshot, RuntimeLifecycle, SessionId, SessionSummary};

/// 创建一个空 Session。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSessionRequest {
    /// 可选展示标题；`None` 表示由 Runtime 选择默认标题。
    pub title: Option<String>,
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
}
