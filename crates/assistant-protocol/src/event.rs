//! Runtime 已发生事实的实时观察事件。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ApprovalDecision, ApprovalId, ApprovalSnapshot, ChildTaskId, ChildTaskSnapshot,
    ChildTaskStatus, ConversationOwner, GuardrailKind, GuardrailMode, ModelFailureKind, PartId,
    PermissionFileSummary, RunId, RunStatus, RuntimeErrorInfo, SessionId, SessionSummary,
    TokenUsageSnapshot, ToolActivityStatus, ToolCallId, ToolOutputChannel, WorkspaceId,
};

/// Runtime 单实例内严格递增的实时事件封装。
///
/// `sequence` 只用于把事件与 [`crate::ObservedSnapshot`] 的水位对齐，不跨 Runtime 重启持久化。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeEventEnvelope {
    pub sequence: u64,
    pub emitted_at_ms: i64,
    pub event: RuntimeEvent,
}

/// 子任务事件的 payload；父子所有权固定放在外层 RuntimeEvent envelope。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChildTaskEvent {
    Created {
        task: Box<ChildTaskSnapshot>,
    },
    Started,
    StepStarted {
        step: u32,
    },
    TextDelta {
        part_id: PartId,
        delta: String,
    },
    ReasoningDelta {
        part_id: PartId,
        delta: String,
    },
    UsageUpdated {
        step: u32,
        usage: TokenUsageSnapshot,
    },
    ToolProposed {
        call_id: ToolCallId,
        tool_name: String,
    },
    ToolStarted {
        call_id: ToolCallId,
    },
    ToolOutput {
        call_id: ToolCallId,
        channel: ToolOutputChannel,
        chunk: String,
    },
    ToolCompleted {
        call_id: ToolCallId,
        status: ToolActivityStatus,
    },
    Finished {
        status: ChildTaskStatus,
        error: Option<RuntimeErrorInfo>,
    },
}

/// Runtime 向在线客户端发布的产品层观察事件。
///
/// 事件允许因背压或断线丢失，客户端必须用 Session/Run 快照重新对齐。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// Runtime 已开始受控关闭。
    RuntimeShuttingDown,
    /// 应用配置投影已经变化；客户端应重新获取 ApplicationSnapshot。
    ConfigChanged,
    /// Workspace Registry 已经变化；客户端应重新获取 ApplicationSnapshot。
    WorkspaceChanged {
        workspace_id: WorkspaceId,
    },
    /// Session 的稳定产品投影已经变化；事件本身不替代组合快照。
    SessionChanged {
        session_id: SessionId,
    },
    /// Session 输入队列已经变化；revision 用于拒绝基于旧顺序的 mutation。
    QueueChanged {
        session_id: SessionId,
        revision: u64,
    },
    /// 规范 Conversation 已经可靠提交；客户端应按 owner/generation 失效历史页。
    ConversationCommitted {
        owner: ConversationOwner,
        generation: u64,
    },
    /// 权限 Registry 已经完成原子替换。
    PermissionChanged,
    /// 一个 Session 已完整创建。
    SessionCreated {
        /// 新 Session 的稳定摘要。
        session: SessionSummary,
    },
    /// 一个 Session 已从 Runtime 权威状态和私有存储中移除。
    SessionDeleted {
        session_id: SessionId,
    },
    /// Session 当前 Agent 变体发生变化。
    SessionVariantChanged {
        session: SessionSummary,
    },
    /// Session 当前审批模式发生变化。
    SessionApprovalModeChanged {
        session: SessionSummary,
    },
    /// 一个 Session cohort 的权限快照已被原子替换。
    PermissionReloaded {
        session_id: SessionId,
        files: Vec<PermissionFileSummary>,
    },
    ApprovalRequested {
        approval: Box<ApprovalSnapshot>,
    },
    ApprovalResolved {
        session_id: SessionId,
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_task_id: Option<ChildTaskId>,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    ApprovalCancelled {
        session_id: SessionId,
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_task_id: Option<ChildTaskId>,
        approval_id: ApprovalId,
    },
    ChildTaskEvent {
        session_id: SessionId,
        parent_run_id: RunId,
        child_task_id: ChildTaskId,
        event: ChildTaskEvent,
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
    /// 当前 Run 开始一个新的模型 Turn；仅表达增量分组边界，不要求 UI 显示 step 文本。
    StepStarted {
        session_id: SessionId,
        run_id: RunId,
        step: u32,
    },
    /// 一次真实模型请求 attempt 即将开始。
    ModelAttemptStarted {
        session_id: SessionId,
        run_id: RunId,
        /// 当前逻辑模型调用内从 1 开始的 attempt。
        attempt: u32,
    },
    /// 一次模型请求 attempt 在建立事件流前失败。
    ModelAttemptFailed {
        session_id: SessionId,
        run_id: RunId,
        attempt: u32,
        kind: ModelFailureKind,
        /// 当前冻结策略是否已经安排后续 attempt。
        will_retry: bool,
    },
    /// 模型服务已经确定下一 attempt 及等待时间。
    ModelRetryScheduled {
        session_id: SessionId,
        run_id: RunId,
        next_attempt: u32,
        delay_ms: u64,
    },
    /// 当前 attempt 已经取得 Provider 事件流；透明重试边界到此结束。
    ModelStreamEstablished {
        session_id: SessionId,
        run_id: RunId,
        attempt: u32,
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
    /// 一个完整模型请求最终确认了 token 用量。
    UsageUpdated {
        /// Run 所属 Session。
        session_id: SessionId,
        /// 模型请求所属 Run。
        run_id: RunId,
        /// 当前 Run 中的模型请求序号，从 1 开始。
        step: u32,
        /// 本次模型请求的最终 Provider 用量。
        usage: TokenUsageSnapshot,
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
    /// 一个 Runtime 配置的 Guardrail 首次达到当前连续序列阈值。
    GuardrailTriggered {
        session_id: SessionId,
        run_id: RunId,
        call_id: ToolCallId,
        kind: GuardrailKind,
        mode: GuardrailMode,
        threshold: u32,
        observed: u32,
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
    use crate::ModelKey;

    #[test]
    fn event_envelope_has_a_stable_watermark_shape() {
        let envelope = RuntimeEventEnvelope {
            sequence: 42,
            emitted_at_ms: 1_700_000_000_000,
            event: RuntimeEvent::RuntimeShuttingDown,
        };
        let value = serde_json::to_value(&envelope).expect("serialize envelope");
        assert_eq!(value["sequence"], 42);
        assert_eq!(value["emitted_at_ms"], 1_700_000_000_000_i64);
        assert_eq!(value["event"]["type"], "runtime_shutting_down");
        assert_eq!(
            serde_json::from_value::<RuntimeEventEnvelope>(value).expect("deserialize envelope"),
            envelope
        );
    }

    fn session_id() -> SessionId {
        SessionId::new("session-1").expect("session id")
    }

    fn run_id() -> RunId {
        RunId::new("run-1").expect("run id")
    }

    fn session_fixture() -> SessionSummary {
        SessionSummary {
            session_id: session_id(),
            title: "Session 1".to_owned(),
            model_key: ModelKey::new("model-1").expect("model key"),
            reasoning_effort: None,
            lifecycle: crate::SessionLifecycle::Active,
            current_variant: crate::AgentVariant::Build,
            approval_mode: crate::ApprovalMode::Ask,
            workspace_id: None,
            active_run_id: None,
            message_count: 0,
            queued_input_count: 0,
            resume_required: false,
            created_at_ms: None,
            updated_at_ms: None,
            archived_at_ms: None,
            is_pinned: false,
            title_origin: Default::default(),
            pending_approval_count: 0,
            active_child_count: 0,
            active_run_status: None,
        }
    }

    fn approval_fixture() -> ApprovalSnapshot {
        ApprovalSnapshot {
            approval_id: ApprovalId::new("approval-1").expect("approval id"),
            session_id: session_id(),
            run_id: run_id(),
            child_task_id: None,
            call_id: ToolCallId::new("call-1").expect("call id"),
            variant: crate::AgentVariant::Build,
            approval_mode: crate::ApprovalMode::Ask,
            subject: crate::ToolApprovalSubject::General {
                tool_name: "echo_text".to_owned(),
            },
            available_decisions: vec![ApprovalDecision::AllowOnce, ApprovalDecision::Deny],
            exact_rule_preview: crate::ToolApprovalSubject::General {
                tool_name: "echo_text".to_owned(),
            },
            status: crate::ApprovalStatus::Pending,
            created_at_ms: 1,
        }
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
                    session: session_fixture(),
                },
                "session_created",
            ),
            (
                RuntimeEvent::SessionDeleted {
                    session_id: session_id(),
                },
                "session_deleted",
            ),
            (
                RuntimeEvent::SessionVariantChanged {
                    session: SessionSummary {
                        current_variant: crate::AgentVariant::Plan,
                        ..session_fixture()
                    },
                },
                "session_variant_changed",
            ),
            (
                RuntimeEvent::SessionApprovalModeChanged {
                    session: SessionSummary {
                        approval_mode: crate::ApprovalMode::Auto,
                        ..session_fixture()
                    },
                },
                "session_approval_mode_changed",
            ),
            (
                RuntimeEvent::PermissionReloaded {
                    session_id: session_id(),
                    files: vec![crate::PermissionFileSummary {
                        scope: crate::PermissionScope::Global,
                        status: crate::PermissionFileStatus::Ready,
                    }],
                },
                "permission_reloaded",
            ),
            (
                RuntimeEvent::ApprovalRequested {
                    approval: Box::new(approval_fixture()),
                },
                "approval_requested",
            ),
            (
                RuntimeEvent::ApprovalResolved {
                    session_id: session_id(),
                    run_id: run_id(),
                    child_task_id: None,
                    approval_id: ApprovalId::new("approval-1").expect("approval id"),
                    decision: ApprovalDecision::AllowSession,
                },
                "approval_resolved",
            ),
            (
                RuntimeEvent::ApprovalCancelled {
                    session_id: session_id(),
                    run_id: run_id(),
                    child_task_id: None,
                    approval_id: ApprovalId::new("approval-1").expect("approval id"),
                },
                "approval_cancelled",
            ),
            (
                RuntimeEvent::ChildTaskEvent {
                    session_id: session_id(),
                    parent_run_id: run_id(),
                    child_task_id: ChildTaskId::new("child-1").expect("child id"),
                    event: ChildTaskEvent::Started,
                },
                "child_task_event",
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
                RuntimeEvent::StepStarted {
                    session_id: session_id(),
                    run_id: run_id(),
                    step: 1,
                },
                "step_started",
            ),
            (
                RuntimeEvent::ModelAttemptStarted {
                    session_id: session_id(),
                    run_id: run_id(),
                    attempt: 1,
                },
                "model_attempt_started",
            ),
            (
                RuntimeEvent::ModelAttemptFailed {
                    session_id: session_id(),
                    run_id: run_id(),
                    attempt: 1,
                    kind: ModelFailureKind::ServiceUnavailable,
                    will_retry: true,
                },
                "model_attempt_failed",
            ),
            (
                RuntimeEvent::ModelRetryScheduled {
                    session_id: session_id(),
                    run_id: run_id(),
                    next_attempt: 2,
                    delay_ms: 500,
                },
                "model_retry_scheduled",
            ),
            (
                RuntimeEvent::ModelStreamEstablished {
                    session_id: session_id(),
                    run_id: run_id(),
                    attempt: 2,
                },
                "model_stream_established",
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
                RuntimeEvent::UsageUpdated {
                    session_id: session_id(),
                    run_id: run_id(),
                    step: 1,
                    usage: TokenUsageSnapshot {
                        input_tokens: 120,
                        output_tokens: 30,
                        total_tokens: 150,
                        cached_input_tokens: Some(80),
                    },
                },
                "usage_updated",
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
                RuntimeEvent::GuardrailTriggered {
                    session_id: session_id(),
                    run_id: run_id(),
                    call_id: ToolCallId::new("call-1").expect("call id"),
                    kind: GuardrailKind::RepeatedInvocation,
                    mode: GuardrailMode::Enforce,
                    threshold: 4,
                    observed: 4,
                },
                "guardrail_triggered",
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
