//! Demo 私有的内存规范对话 Journal 与两阶段 Recorder。

use std::{
    future::ready,
    sync::{Arc, Mutex, MutexGuard},
};

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{
    AssistantMessage, ConversationMessage, ConversationSnapshot, ToolMessage, UserMessage,
};
use thiserror::Error;

use crate::audit::DemoAudit;

#[derive(Default)]
struct JournalState {
    messages: Vec<ConversationMessage>,
    pending: Vec<PendingExchange>,
    next_exchange: u64,
}

struct PendingExchange {
    receipt: ExchangeReceipt,
    run_id: String,
    assistant: AssistantMessage,
}

#[derive(Clone, Default)]
pub(crate) struct DemoJournal {
    state: Arc<Mutex<JournalState>>,
}

impl DemoJournal {
    pub(crate) fn append_user(&self, message: UserMessage) {
        self.lock()
            .messages
            .push(ConversationMessage::User(message));
    }

    pub(crate) fn append_assistant(&self, message: AssistantMessage) {
        self.lock()
            .messages
            .push(ConversationMessage::Assistant(message));
    }

    pub(crate) fn snapshot(&self) -> ConversationSnapshot {
        ConversationSnapshot::new(self.lock().messages.clone())
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.lock().pending.is_empty()
    }

    pub(crate) fn clear(&self) {
        let mut state = self.lock();
        state.messages.clear();
        state.pending.clear();
    }

    fn begin(
        &self,
        run_id: &str,
        assistant: AssistantMessage,
    ) -> Result<ExchangeReceipt, JournalError> {
        let mut state = self.lock();
        state.next_exchange = state.next_exchange.saturating_add(1);
        let receipt = ExchangeReceipt::new(format!("{run_id}-exchange-{}", state.next_exchange))
            .map_err(JournalError::InvalidReceipt)?;
        state.pending.push(PendingExchange {
            receipt: receipt.clone(),
            run_id: run_id.to_owned(),
            assistant,
        });
        Ok(receipt)
    }

    fn complete(
        &self,
        run_id: &str,
        receipt: &ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> Result<(), JournalError> {
        let mut state = self.lock();
        let index = state
            .pending
            .iter()
            .position(|pending| pending.receipt == *receipt)
            .ok_or_else(|| JournalError::UnknownReceipt(receipt.as_str().to_owned()))?;
        if state.pending[index].run_id != run_id {
            return Err(JournalError::RunMismatch);
        }
        let pending = state.pending.remove(index);
        state
            .messages
            .push(ConversationMessage::Assistant(pending.assistant));
        state
            .messages
            .extend(results.into_iter().map(ConversationMessage::Tool));
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, JournalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct DemoRecorder {
    journal: DemoJournal,
    audit: DemoAudit,
    run_id: String,
}

impl DemoRecorder {
    pub(crate) fn new(journal: DemoJournal, audit: DemoAudit, run_id: String) -> Arc<Self> {
        Arc::new(Self {
            journal,
            audit,
            run_id,
        })
    }
}

impl ExecutionRecorder for DemoRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(ready(
            self.journal
                .begin(&self.run_id, assistant)
                .map_err(record_error),
        ))
    }

    fn mark_tool_execution_started<'a>(
        &'a self,
        _receipt: &'a ExchangeReceipt,
        _call_id: &'a agent_types::ToolCallId,
    ) -> RecordFuture<'a, ()> {
        Box::pin(ready(Ok(())))
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        for result in &results {
            self.audit.record_result(&self.run_id, &result.result);
        }
        Box::pin(ready(
            self.journal
                .complete(&self.run_id, receipt, results)
                .map_err(record_error),
        ))
    }
}

fn record_error(error: JournalError) -> RecordError {
    RecordError {
        message: error.to_string(),
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
enum JournalError {
    #[error("failed to create exchange receipt")]
    InvalidReceipt(RecordError),
    #[error("unknown pending exchange `{0}`")]
    UnknownReceipt(String),
    #[error("pending exchange belongs to another run")]
    RunMismatch,
}

#[cfg(test)]
mod tests {
    use agent_types::{
        FinishReason, MessageId, ModelIdentity, ProviderId, ToolCallId, ToolResult,
        ToolResultContent, ToolResultStatus,
    };

    use super::*;

    fn assistant() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant-1").expect("id"),
            model: ModelIdentity::new(ProviderId::new("test").expect("provider"), "model"),
            parts: vec![],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    #[tokio::test]
    async fn pending_exchange_is_hidden_until_complete() {
        let journal = DemoJournal::default();
        let recorder = DemoRecorder::new(journal.clone(), DemoAudit::default(), "run-1".to_owned());
        let receipt = recorder
            .begin_tool_exchange(assistant())
            .await
            .expect("begin");
        assert!(journal.snapshot().messages.is_empty());
        let result = ToolMessage {
            id: MessageId::new("tool-1").expect("id"),
            result: ToolResult {
                call_id: ToolCallId::new("call-1").expect("call"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("ok".to_owned()),
            },
        };
        recorder
            .complete_tool_exchange(&receipt, vec![result])
            .await
            .expect("complete");
        assert_eq!(journal.snapshot().messages.len(), 2);
    }
}
