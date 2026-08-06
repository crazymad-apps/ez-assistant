//! Runtime 已发生事实的实时观察事件。

use serde::{Deserialize, Serialize};

use crate::{
    PartId, RunId, RunStatus, RuntimeErrorInfo, SessionId, SessionSummary, ToolActivityStatus,
    ToolCallId, ToolOutputChannel,
};

/// Runtime 向在线客户端发布的产品层观察事件。
///
/// 事件允许因背压或断线丢失，客户端必须用 Session/Run 快照重新对齐。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// Runtime 已开始受控关闭。
    RuntimeShuttingDown,
    /// 一个 Session 已完整创建。
    SessionCreated {
        /// 新 Session 的稳定摘要。
        session: SessionSummary,
    },
    /// 一个 Run 已被 Runtime 原子登记。
    RunAccepted {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 已接受的 Run。
        run_id: RunId,
    },
    /// 一个 Run 的 AgentExecution 已开始。
    RunStarted {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 已开始的 Run。
        run_id: RunId,
    },
    /// Runtime 已接受 Run 取消请求。
    RunCancelling {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 正在取消的 Run。
        run_id: RunId,
    },
    /// Run 产生正文文本增量。
    TextDelta {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 产生增量的 Run。
        run_id: RunId,
        /// 文本片段的不透明标识。
        part_id: PartId,
        /// 本次增量内容。
        delta: String,
    },
    /// Run 产生 reasoning 文本增量。
    ReasoningDelta {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 产生增量的 Run。
        run_id: RunId,
        /// reasoning 片段的不透明标识。
        part_id: PartId,
        /// 本次增量内容。
        delta: String,
    },
    /// 模型提出一个工具调用。
    ToolProposed {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 工具调用所属 Run。
        run_id: RunId,
        /// 工具调用的不透明标识。
        call_id: ToolCallId,
        /// 模型可见工具名；不携带原始参数。
        tool_name: String,
    },
    /// 工具调用已通过授权并开始执行。
    ToolStarted {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 工具调用所属 Run。
        run_id: RunId,
        /// 工具调用的不透明标识。
        call_id: ToolCallId,
    },
    /// 工具调用产生流式输出。
    ToolOutput {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 工具调用所属 Run。
        run_id: RunId,
        /// 工具调用的不透明标识。
        call_id: ToolCallId,
        /// 输出通道。
        channel: ToolOutputChannel,
        /// 本次输出片段。
        chunk: String,
    },
    /// 工具调用已经完成。
    ToolCompleted {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 工具调用所属 Run。
        run_id: RunId,
        /// 工具调用的不透明标识。
        call_id: ToolCallId,
        /// 完成后的工具活动状态。
        status: ToolActivityStatus,
    },
    /// Run 已由 completion 唯一结算为终态。
    RunFinished {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 已结算的 Run。
        run_id: RunId,
        /// 不可再次改变的 Run 终态。
        status: RunStatus,
        /// Run 失败时的脱敏错误。
        error: Option<RuntimeErrorInfo>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session_id() -> SessionId {
        SessionId::new("session-1").expect("session id")
    }

    fn run_id() -> RunId {
        RunId::new("run-1").expect("run id")
    }

    #[test]
    fn text_delta_has_explicit_type_and_run_ownership() {
        let event = RuntimeEvent::TextDelta {
            session_id: session_id(),
            run_id: run_id(),
            part_id: PartId::new("part-1").expect("part id"),
            delta: "hello".to_owned(),
        };
        let value = serde_json::to_value(&event).expect("serialize event");

        assert_eq!(
            value,
            json!({
                "type": "text_delta",
                "session_id": "session-1",
                "run_id": "run-1",
                "part_id": "part-1",
                "delta": "hello"
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeEvent>(value).expect("deserialize event"),
            event
        );
    }

    #[test]
    fn run_finished_round_trips_as_a_single_terminal_fact() {
        let event = RuntimeEvent::RunFinished {
            session_id: session_id(),
            run_id: run_id(),
            status: RunStatus::Cancelled,
            error: None,
        };
        let json = serde_json::to_string(&event).expect("serialize event");

        assert_eq!(
            serde_json::from_str::<RuntimeEvent>(&json).expect("deserialize event"),
            event
        );
    }

    #[test]
    fn every_event_variant_has_a_stable_tag_and_round_trips() {
        let events = vec![
            (RuntimeEvent::RuntimeShuttingDown, "runtime_shutting_down"),
            (
                RuntimeEvent::SessionCreated {
                    session: SessionSummary {
                        session_id: session_id(),
                        title: "Session 1".to_owned(),
                        active_run_id: None,
                        message_count: 0,
                    },
                },
                "session_created",
            ),
            (
                RuntimeEvent::RunAccepted {
                    session_id: session_id(),
                    run_id: run_id(),
                },
                "run_accepted",
            ),
            (
                RuntimeEvent::RunStarted {
                    session_id: session_id(),
                    run_id: run_id(),
                },
                "run_started",
            ),
            (
                RuntimeEvent::RunCancelling {
                    session_id: session_id(),
                    run_id: run_id(),
                },
                "run_cancelling",
            ),
            (
                RuntimeEvent::TextDelta {
                    session_id: session_id(),
                    run_id: run_id(),
                    part_id: PartId::new("part-1").expect("part id"),
                    delta: "text".to_owned(),
                },
                "text_delta",
            ),
            (
                RuntimeEvent::ReasoningDelta {
                    session_id: session_id(),
                    run_id: run_id(),
                    part_id: PartId::new("part-2").expect("part id"),
                    delta: "reasoning".to_owned(),
                },
                "reasoning_delta",
            ),
            (
                RuntimeEvent::ToolProposed {
                    session_id: session_id(),
                    run_id: run_id(),
                    call_id: ToolCallId::new("call-1").expect("call id"),
                    tool_name: "echo_text".to_owned(),
                },
                "tool_proposed",
            ),
            (
                RuntimeEvent::ToolStarted {
                    session_id: session_id(),
                    run_id: run_id(),
                    call_id: ToolCallId::new("call-1").expect("call id"),
                },
                "tool_started",
            ),
            (
                RuntimeEvent::ToolOutput {
                    session_id: session_id(),
                    run_id: run_id(),
                    call_id: ToolCallId::new("call-1").expect("call id"),
                    channel: ToolOutputChannel::Stdout,
                    chunk: "hello".to_owned(),
                },
                "tool_output",
            ),
            (
                RuntimeEvent::ToolCompleted {
                    session_id: session_id(),
                    run_id: run_id(),
                    call_id: ToolCallId::new("call-1").expect("call id"),
                    status: ToolActivityStatus::Completed,
                },
                "tool_completed",
            ),
            (
                RuntimeEvent::RunFinished {
                    session_id: session_id(),
                    run_id: run_id(),
                    status: RunStatus::Completed,
                    error: None,
                },
                "run_finished",
            ),
        ];

        for (event, tag) in events {
            let value = serde_json::to_value(&event).expect("serialize event");
            assert_eq!(value["type"], tag);
            assert_eq!(
                serde_json::from_value::<RuntimeEvent>(value).expect("deserialize event"),
                event
            );
        }
    }
}
