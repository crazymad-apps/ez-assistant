//! 子任务执行结果到持久化终态及父 Tool Result 的映射。

use agent_core::{ExecutionError, ExecutionOutcome};
use agent_tools::ToolError;
use agent_types::{AssistantPart, ConversationMessage};
use assistant_protocol::{ChildTaskStatus, RuntimeErrorCode, RuntimeErrorInfo};
use serde_json::json;

use super::{ChildTaskRecord, ChildTaskRegistry, cancellation::ChildCancellationReason};
use crate::{RuntimeStore, StoredChildTask, StoredChildTaskSettlement, id};

pub(super) struct ChildTerminal {
    pub(super) status: ChildTaskStatus,
    pub(super) cancel_requested: bool,
    pub(super) error: Option<RuntimeErrorInfo>,
    pub(super) messages: Vec<ConversationMessage>,
    pub(super) final_message_id: Option<agent_types::MessageId>,
    pub(super) result: Option<String>,
}

/// Completion 只有在独立 Journal 不含 pending tool exchange 时才可成为可靠终态。
pub(super) fn child_terminal(
    outcome: ExecutionOutcome,
    task: &ChildTaskRecord,
    cancellation_reason: Option<ChildCancellationReason>,
) -> ChildTerminal {
    let has_pending = task
        .lock_state()
        .map(|state| {
            state
                .journal
                .as_ref()
                .is_some_and(|journal| journal.has_pending())
        })
        .unwrap_or(true);
    if has_pending {
        return failed_terminal(RuntimeErrorInfo::new(
            RuntimeErrorCode::Internal,
            "child task ended with an incomplete tool exchange",
        ));
    }
    if cancellation_reason == Some(ChildCancellationReason::Timeout) {
        return failed_terminal(RuntimeErrorInfo::new(
            RuntimeErrorCode::Timeout,
            "child task exceeded its execution timeout",
        ));
    }
    match outcome {
        ExecutionOutcome::Completed(message) => {
            let result = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(text) => Some(text.text.as_str()),
                    AssistantPart::Reasoning(_)
                    | AssistantPart::ToolCall(_)
                    | AssistantPart::ProviderState(_) => None,
                })
                .collect::<String>();
            let message_id = message.id.clone();
            ChildTerminal {
                status: ChildTaskStatus::Completed,
                cancel_requested: cancellation_reason.is_some(),
                error: None,
                messages: vec![ConversationMessage::Assistant(message)],
                final_message_id: Some(message_id),
                result: Some(result),
            }
        }
        ExecutionOutcome::Cancelled => ChildTerminal {
            status: ChildTaskStatus::Cancelled,
            cancel_requested: true,
            error: None,
            messages: Vec::new(),
            final_message_id: None,
            result: None,
        },
        ExecutionOutcome::Failed(error) => failed_terminal(execution_error(error)),
        ExecutionOutcome::CompactionRequired { .. } => failed_terminal(RuntimeErrorInfo::new(
            RuntimeErrorCode::Internal,
            "child task requires context compaction",
        )),
    }
}

pub(super) fn child_terminal_with_error(error: RuntimeErrorInfo) -> ChildTerminal {
    failed_terminal(error)
}

pub(super) fn child_error(stored: &StoredChildTask, error: RuntimeErrorInfo) -> ToolError {
    ToolError::execution_with_details(
        "child task did not complete",
        json!({
            "task_id": stored.child_task_id.as_str(),
            "status": child_status_value(stored.status),
            "code": error.code,
        }),
    )
}

/// 在初始 User Message 落盘前把 accepted 关系收敛为失败或取消终态。
pub(super) async fn settle_accepted(
    store: &dyn RuntimeStore,
    registry: &ChildTaskRegistry,
    stored: &mut StoredChildTask,
    status: ChildTaskStatus,
    error: Option<RuntimeErrorInfo>,
) -> Result<(), ToolError> {
    debug_assert!(matches!(
        status,
        ChildTaskStatus::Failed | ChildTaskStatus::Cancelled
    ));
    let operation_id = id::generate("append")
        .map_err(|_| ToolError::execution("child pre-start settlement id is unavailable"))?;
    let finished_at_ms = crate::runtime::now_ms()
        .map_err(|_| ToolError::execution("child pre-start settlement time is unavailable"))?;
    let cancel_requested = status == ChildTaskStatus::Cancelled;
    store
        .settle_child_task(StoredChildTaskSettlement {
            operation_id,
            child_task_id: stored.child_task_id.clone(),
            session_id: stored.session_id.clone(),
            status,
            cancel_requested,
            error: error.clone(),
            messages: Vec::new(),
            final_message_id: None,
            finished_at_ms,
        })
        .await
        .map_err(|_| ToolError::execution("child pre-start settlement could not be persisted"))?;
    stored.status = status;
    stored.cancel_requested |= cancel_requested;
    stored.error = error;
    stored.finished_at_ms = Some(finished_at_ms);
    registry
        .upsert(stored.clone())
        .map_err(|_| ToolError::execution("child task runtime state is unavailable"))
}

fn failed_terminal(error: RuntimeErrorInfo) -> ChildTerminal {
    ChildTerminal {
        status: ChildTaskStatus::Failed,
        cancel_requested: false,
        error: Some(error),
        messages: Vec::new(),
        final_message_id: None,
        result: None,
    }
}

fn execution_error(error: ExecutionError) -> RuntimeErrorInfo {
    let (code, message) = match error {
        ExecutionError::Model(_) => (
            RuntimeErrorCode::ModelExecutionFailed,
            "child model execution failed",
        ),
        ExecutionError::BudgetExceeded { .. } => (
            RuntimeErrorCode::Internal,
            "child execution budget was exceeded",
        ),
        ExecutionError::GuardrailTriggered { .. } => (
            RuntimeErrorCode::Internal,
            "child execution guardrail was triggered",
        ),
        ExecutionError::ContextWindow(_) => (
            RuntimeErrorCode::Internal,
            "child conversation context is invalid",
        ),
        ExecutionError::Record(_) => (
            RuntimeErrorCode::Internal,
            "child conversation could not be recorded",
        ),
        ExecutionError::Internal => (
            RuntimeErrorCode::Internal,
            "child execution task terminated unexpectedly",
        ),
    };
    RuntimeErrorInfo::new(code, message)
}

fn child_status_value(status: ChildTaskStatus) -> &'static str {
    match status {
        ChildTaskStatus::Accepted => "accepted",
        ChildTaskStatus::Running => "running",
        ChildTaskStatus::Completed => "completed",
        ChildTaskStatus::Failed => "failed",
        ChildTaskStatus::Cancelled => "cancelled",
        ChildTaskStatus::Interrupted => "interrupted",
    }
}
