//! 主 Run 与子任务 Conversation 的 Recorder 目标适配。
//!
//! 这里集中目标特有的所有权校验、Store DTO 和内存 Journal 提交；上层
//! `RuntimeRecorder` 只保留 begin/started/complete 三段公共算法。

use std::sync::Arc;

use agent_core::{ExchangeReceipt, RecordError};
use agent_types::{AssistantMessage, ConversationMessage, ToolMessage, UserMessage};
use assistant_protocol::{ConversationOwner, RunId, ToolCallId as ProtocolToolCallId};

use crate::{
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange,
    PendingChildToolExchange, PendingToolExchange, RuntimeStore, ToolExecutionStart,
    delegation::{ChildTaskJournalState, ChildTaskRecord},
    run::is_active_run,
    session::{SessionController, SessionState},
};

use super::record_error;

/// Recorder 已冻结、等待 Store 原子提交的一次完整 Tool Exchange。
pub(super) struct PersistedToolExchangeCompletion {
    pub(super) operation_id: String,
    pub(super) receipt: ExchangeReceipt,
    pub(super) step: u32,
    pub(super) results: Vec<ToolMessage>,
    pub(super) activation_message: Option<UserMessage>,
    pub(super) skill_activations: Vec<crate::StoredSkillActivation>,
    pub(super) completed_at_ms: i64,
}

/// Recorder 只在这一处区分主 Run 与 child；Core 契约和可靠顺序完全共用。
pub(super) enum RecorderTarget {
    Parent {
        session: Arc<SessionController>,
        run_id: RunId,
    },
    Child {
        task: Arc<ChildTaskRecord>,
        session: Arc<SessionController>,
    },
}

impl RecorderTarget {
    pub(super) fn parent(session: Arc<SessionController>, run_id: RunId) -> Self {
        Self::Parent { session, run_id }
    }

    pub(super) fn child(task: Arc<ChildTaskRecord>, session: Arc<SessionController>) -> Self {
        Self::Child { task, session }
    }

    pub(super) async fn mutation(&self) -> tokio::sync::MutexGuard<'_, ()> {
        match self {
            Self::Parent { session, .. } => session.mutation().await,
            Self::Child { task, .. } => task.mutation().await,
        }
    }

    pub(super) fn validate_begin(&self, assistant: &AssistantMessage) -> Result<(), RecordError> {
        match self {
            Self::Parent { session, run_id } => {
                let mut state = lock_parent(session)?;
                if !is_active_run(&state, run_id) {
                    state.is_faulted = true;
                    return Err(record_error("active run does not match recorder"));
                }
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                journal
                    .validate_tool_exchange_begin(assistant)
                    .map_err(|_| record_error("journal rejected tool exchange begin"))
            }
            Self::Child { task, .. } => {
                let state = lock_child(task)?;
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("child conversation is unavailable"))?;
                journal
                    .validate_tool_exchange_begin(assistant)
                    .map_err(|_| record_error("child journal rejected tool exchange begin"))
            }
        }
    }

    pub(super) fn commit_begin(
        &self,
        receipt: ExchangeReceipt,
        _step: u32,
        assistant: AssistantMessage,
    ) -> Result<(), RecordError> {
        match self {
            Self::Parent { session, run_id } => {
                let mut state = lock_parent(session)?;
                let journal = state
                    .journal
                    .as_mut()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                journal
                    .begin_tool_exchange_with_receipt(run_id, receipt, assistant)
                    .map_err(|_| record_error("journal rejected persisted tool exchange"))
            }
            Self::Child { task, .. } => {
                let mut state = lock_child(task)?;
                let journal = state
                    .journal
                    .as_mut()
                    .ok_or_else(|| record_error("child conversation is unavailable"))?;
                journal
                    .begin_tool_exchange_with_receipt(task.journal_owner(), receipt, assistant)
                    .map_err(|_| record_error("child journal rejected persisted tool exchange"))
            }
        }
    }

    pub(super) fn validate_complete(
        &self,
        receipt: &ExchangeReceipt,
        results: &[ToolMessage],
        activation_message: Option<&UserMessage>,
    ) -> Result<Vec<ConversationMessage>, RecordError> {
        let trailing = activation_message
            .cloned()
            .map(ConversationMessage::User)
            .into_iter()
            .collect::<Vec<_>>();
        match self {
            Self::Parent { session, run_id } => {
                let mut state = lock_parent(session)?;
                if !is_active_run(&state, run_id) {
                    state.is_faulted = true;
                    return Err(record_error("active run does not match recorder"));
                }
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                journal
                    .tool_exchange_batch_with_trailing(run_id, receipt, results, &trailing)
                    .map_err(|_| record_error("journal rejected tool exchange completion"))
            }
            Self::Child { task, .. } => {
                let state = lock_child(task)?;
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("child conversation is unavailable"))?;
                journal
                    .tool_exchange_batch_with_trailing(
                        task.journal_owner(),
                        receipt,
                        results,
                        &trailing,
                    )
                    .map_err(|_| record_error("child journal rejected tool exchange completion"))
            }
        }
    }

    pub(super) fn commit_complete(
        &self,
        receipt: &ExchangeReceipt,
        step: u32,
        results: Vec<ToolMessage>,
        activation_message: Option<UserMessage>,
        skill_activations: Vec<crate::StoredSkillActivation>,
        batch: &[ConversationMessage],
    ) -> Result<(), RecordError> {
        match self {
            Self::Parent { session, run_id } => {
                let message_ids = batch.iter().map(message_id).cloned().collect::<Vec<_>>();
                let mut state = lock_parent(session)?;
                let journal = state
                    .journal
                    .as_mut()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                journal
                    .complete_tool_exchange_with_trailing(
                        run_id,
                        receipt,
                        results,
                        activation_message
                            .map(ConversationMessage::User)
                            .into_iter()
                            .collect(),
                    )
                    .map_err(|_| record_error("journal rejected persisted tool exchange"))?;
                let persisted_message_count = journal.message_count();
                let message_count = u64::try_from(persisted_message_count)
                    .map_err(|_| record_error("conversation message count is exhausted"))?;
                let run = state
                    .runs
                    .get_mut(run_id)
                    .ok_or_else(|| record_error("active run record is unavailable"))?;
                run.extend_message_ids_at_step(message_ids, step);
                state.skill_activations.extend(skill_activations);
                state.persisted_message_count = persisted_message_count;
                state.message_count = message_count;
                Ok(())
            }
            Self::Child { task, session } => {
                let mut state = lock_child(task)?;
                let journal = state
                    .journal
                    .as_mut()
                    .ok_or_else(|| record_error("child conversation is unavailable"))?;
                journal
                    .complete_tool_exchange_with_trailing(
                        task.journal_owner(),
                        receipt,
                        results,
                        activation_message
                            .map(ConversationMessage::User)
                            .into_iter()
                            .collect(),
                    )
                    .map_err(|_| record_error("child journal rejected persisted tool exchange"))?;
                state.persisted_message_count = journal.message_count();
                drop(state);
                lock_parent(session)?
                    .skill_activations
                    .extend(skill_activations);
                Ok(())
            }
        }
    }

    pub(super) async fn persist_begin(
        &self,
        store: &dyn RuntimeStore,
        receipt: ExchangeReceipt,
        step: u32,
        assistant: AssistantMessage,
        created_at_ms: i64,
    ) -> Result<(), crate::StoreError> {
        match self {
            Self::Parent { session, run_id } => {
                store
                    .begin_tool_exchange(PendingToolExchange {
                        receipt,
                        session_id: session.id().clone(),
                        run_id: run_id.clone(),
                        step,
                        assistant,
                        created_at_ms,
                    })
                    .await
            }
            Self::Child { task, .. } => {
                store
                    .begin_child_tool_exchange(PendingChildToolExchange {
                        receipt,
                        child_task_id: task.id().clone(),
                        session_id: task.session_id().clone(),
                        step,
                        assistant,
                        created_at_ms,
                    })
                    .await
            }
        }
    }

    pub(super) async fn persist_started(
        &self,
        store: &dyn RuntimeStore,
        receipt: ExchangeReceipt,
        call_id: ProtocolToolCallId,
        started_at_ms: i64,
    ) -> Result<(), crate::StoreError> {
        match self {
            Self::Parent { session, run_id } => {
                store
                    .mark_tool_execution_started(ToolExecutionStart {
                        receipt,
                        session_id: session.id().clone(),
                        run_id: run_id.clone(),
                        call_id,
                        started_at_ms,
                    })
                    .await
            }
            Self::Child { task, .. } => {
                store
                    .mark_child_tool_execution_started(ChildToolExecutionStart {
                        receipt,
                        child_task_id: task.id().clone(),
                        session_id: task.session_id().clone(),
                        call_id,
                        started_at_ms,
                    })
                    .await
            }
        }
    }

    pub(super) async fn persist_complete(
        &self,
        store: &dyn RuntimeStore,
        completion: PersistedToolExchangeCompletion,
    ) -> Result<(), crate::StoreError> {
        match self {
            Self::Parent { session, run_id } => {
                store
                    .complete_tool_exchange(CompletedToolExchange {
                        operation_id: completion.operation_id,
                        receipt: completion.receipt,
                        session_id: session.id().clone(),
                        run_id: run_id.clone(),
                        step: completion.step,
                        results: completion.results,
                        activation_message: completion.activation_message,
                        skill_activations: completion.skill_activations,
                        completed_at_ms: completion.completed_at_ms,
                    })
                    .await
            }
            Self::Child { task, .. } => {
                store
                    .complete_child_tool_exchange(CompletedChildToolExchange {
                        operation_id: completion.operation_id,
                        receipt: completion.receipt,
                        child_task_id: task.id().clone(),
                        session_id: task.session_id().clone(),
                        step: completion.step,
                        results: completion.results,
                        activation_message: completion.activation_message,
                        skill_activations: completion.skill_activations,
                        completed_at_ms: completion.completed_at_ms,
                    })
                    .await
            }
        }
    }

    pub(super) fn fault(&self) {
        match self {
            Self::Parent { session, .. } => {
                if let Ok(mut state) = session.lock_state() {
                    state.is_faulted = true;
                }
            }
            Self::Child { .. } => {
                // child 的 Recorder 错误会立即收敛当前独立 AgentExecution，并由控制器
                // 可靠写入 failed 终态；它没有接受后续输入的会话级入口，无需第二份 fault flag。
            }
        }
    }

    pub(super) fn committed_projection(&self) -> Result<(ConversationOwner, u64), RecordError> {
        match self {
            Self::Parent { session, .. } => {
                let state = lock_parent(session)?;
                Ok((
                    ConversationOwner::MainSession {
                        session_id: session.id().clone(),
                    },
                    state.body_generation,
                ))
            }
            Self::Child { task, .. } => {
                let state = lock_child(task)?;
                Ok((
                    ConversationOwner::ChildTask {
                        session_id: task.session_id().clone(),
                        child_task_id: task.id().clone(),
                    },
                    state.body_generation,
                ))
            }
        }
    }

    /// 生成模型 Activation ledger 所需的冻结 Conversation 归属事实。
    pub(super) fn skill_activation_context(
        &self,
    ) -> (
        assistant_protocol::SessionId,
        crate::SkillActivationOwner,
        assistant_protocol::RunId,
        String,
    ) {
        match self {
            Self::Parent { session, run_id } => (
                session.id().clone(),
                crate::SkillActivationOwner::Session(session.id().clone()),
                run_id.clone(),
                session.skill_catalog().revision.clone(),
            ),
            Self::Child { task, session } => (
                task.session_id().clone(),
                crate::SkillActivationOwner::ChildTask(task.id().as_str().to_owned()),
                task.journal_owner().clone(),
                session.skill_catalog().revision.clone(),
            ),
        }
    }
}

fn lock_parent(
    session: &SessionController,
) -> Result<std::sync::MutexGuard<'_, SessionState>, RecordError> {
    session
        .lock_state()
        .map_err(|_| record_error("session state is unavailable"))
}

fn lock_child(
    task: &ChildTaskRecord,
) -> Result<std::sync::MutexGuard<'_, ChildTaskJournalState>, RecordError> {
    task.lock_state()
        .map_err(|_| record_error("child task state is unavailable"))
}

fn message_id(message: &ConversationMessage) -> &agent_types::MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}
