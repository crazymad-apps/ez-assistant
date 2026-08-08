//! Core 两阶段 Recorder 到 Runtime Session/Run Journal 的绑定。

use std::sync::Arc;

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{AssistantMessage, ToolMessage};
use assistant_protocol::RunId;

use crate::{
    CompletedToolExchange, PendingToolExchange, RuntimeStore, id,
    session::{SessionController, SessionState},
};

use super::is_active_run;

/// 把 Core 的两阶段落账调用绑定到唯一 Session/Run。
pub(crate) struct RuntimeRecorder {
    session: Arc<SessionController>,
    run_id: RunId,
    store: Arc<dyn RuntimeStore>,
}

impl RuntimeRecorder {
    pub(crate) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        store: Arc<dyn RuntimeStore>,
    ) -> Self {
        Self {
            session,
            run_id,
            store,
        }
    }
}

impl ExecutionRecorder for RuntimeRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            // mutation gate 跨越 Store await，保证终态结算不能越过一个尚未完成的
            // pending 提交；同步 state lock 只用于前后两个短内存临界区。
            let _mutation = self.session.mutation().await;
            {
                let mut state = self.lock_state()?;
                if !is_active_run(&state, &self.run_id) {
                    state.is_faulted = true;
                    return Err(record_error("active run does not match recorder"));
                }
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                if journal.validate_tool_exchange_begin(&assistant).is_err() {
                    state.is_faulted = true;
                    return Err(record_error("journal rejected tool exchange begin"));
                }
            }

            let receipt = ExchangeReceipt::new(
                id::generate("exchange")
                    .map_err(|_| record_error("tool exchange id could not be allocated"))?,
            )?;
            let created_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool exchange time could not be recorded"))?;
            if self
                .store
                .begin_tool_exchange(PendingToolExchange {
                    receipt: receipt.clone(),
                    session_id: self.session.id().clone(),
                    run_id: self.run_id.clone(),
                    assistant: assistant.clone(),
                    created_at_ms,
                })
                .await
                .is_err()
            {
                self.fault_state();
                return Err(record_error("tool exchange begin could not be persisted"));
            }

            let mut state = self.lock_state()?;
            let journal = state
                .journal
                .as_mut()
                .ok_or_else(|| record_error("session conversation is unavailable"))?;
            if journal
                .begin_tool_exchange_with_receipt(&self.run_id, receipt.clone(), assistant)
                .is_err()
            {
                state.is_faulted = true;
                return Err(record_error("journal rejected persisted tool exchange"));
            }
            Ok(receipt)
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let _mutation = self.session.mutation().await;
            let batch = {
                let mut state = self.lock_state()?;
                if !is_active_run(&state, &self.run_id) {
                    state.is_faulted = true;
                    return Err(record_error("active run does not match recorder"));
                }
                let journal = state
                    .journal
                    .as_ref()
                    .ok_or_else(|| record_error("session conversation is unavailable"))?;
                match journal.tool_exchange_batch(&self.run_id, receipt, &results) {
                    Ok(batch) => batch,
                    Err(_) => {
                        state.is_faulted = true;
                        return Err(record_error("journal rejected tool exchange completion"));
                    }
                }
            };
            let operation_id = id::generate("append")
                .map_err(|_| record_error("storage operation id could not be allocated"))?;
            let completed_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool exchange time could not be recorded"))?;
            if self
                .store
                .complete_tool_exchange(CompletedToolExchange {
                    operation_id,
                    receipt: receipt.clone(),
                    session_id: self.session.id().clone(),
                    run_id: self.run_id.clone(),
                    results: results.clone(),
                    completed_at_ms,
                })
                .await
                .is_err()
            {
                self.fault_state();
                return Err(record_error(
                    "tool exchange completion could not be persisted",
                ));
            }

            let message_ids = batch.iter().map(message_id).cloned().collect::<Vec<_>>();
            let mut state = self.lock_state()?;
            let Some(journal) = state.journal.as_mut() else {
                state.is_faulted = true;
                return Err(record_error("session conversation is unavailable"));
            };
            if journal
                .complete_tool_exchange(&self.run_id, receipt, results)
                .is_err()
            {
                state.is_faulted = true;
                return Err(record_error("journal rejected persisted tool exchange"));
            }
            let persisted_message_count = journal.message_count();
            let Ok(message_count) = u64::try_from(persisted_message_count) else {
                state.is_faulted = true;
                return Err(record_error("conversation message count is exhausted"));
            };
            let Some(run) = state.runs.get_mut(&self.run_id) else {
                state.is_faulted = true;
                return Err(record_error("active run record is unavailable"));
            };
            run.extend_message_ids(message_ids);
            state.persisted_message_count = persisted_message_count;
            state.message_count = message_count;
            Ok(())
        })
    }
}

impl RuntimeRecorder {
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, SessionState>, RecordError> {
        self.session
            .lock_state()
            .map_err(|_| record_error("session state is unavailable"))
    }

    fn fault_state(&self) {
        if let Ok(mut state) = self.session.lock_state() {
            state.is_faulted = true;
        }
    }
}

fn message_id(message: &agent_types::ConversationMessage) -> &agent_types::MessageId {
    match message {
        agent_types::ConversationMessage::System(message) => &message.id,
        agent_types::ConversationMessage::ContextSummary(message) => &message.id,
        agent_types::ConversationMessage::User(message) => &message.id,
        agent_types::ConversationMessage::Assistant(message) => &message.id,
        agent_types::ConversationMessage::Tool(message) => &message.id,
    }
}

fn record_error(message: &'static str) -> RecordError {
    RecordError {
        message: message.to_owned(),
    }
}
