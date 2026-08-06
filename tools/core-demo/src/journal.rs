//! Demo 私有的内存 Conversation Journal 与两阶段 Recorder。

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
    completed: Vec<ConversationMessage>,
    pending: Option<PendingExchange>,
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
            .completed
            .push(ConversationMessage::User(message));
    }

    pub(crate) fn append_assistant(&self, message: AssistantMessage) {
        self.lock()
            .completed
            .push(ConversationMessage::Assistant(message));
    }

    pub(crate) fn snapshot(&self) -> ConversationSnapshot {
        ConversationSnapshot::new(self.lock().completed.clone())
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.lock().pending.is_some()
    }

    /// 在无 pending 工具交换时原子提交压缩后的有效快照。
    pub(crate) fn replace_snapshot(
        &self,
        replacement: ConversationSnapshot,
    ) -> Result<(), JournalError> {
        let mut state = self.lock();
        if state.pending.is_some() {
            return Err(JournalError::PendingExchangeExists);
        }
        state.completed = replacement.messages;
        Ok(())
    }

    fn begin(
        &self,
        run_id: &str,
        assistant: AssistantMessage,
    ) -> Result<ExchangeReceipt, JournalError> {
        let mut state = self.lock();
        if state.pending.is_some() {
            return Err(JournalError::PendingExchangeExists);
        }
        state.next_exchange = state.next_exchange.saturating_add(1);
        let receipt = ExchangeReceipt::new(format!("{run_id}-exchange-{}", state.next_exchange))
            .map_err(JournalError::InvalidReceipt)?;
        state.pending = Some(PendingExchange {
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
        let pending = state
            .pending
            .as_ref()
            .ok_or_else(|| JournalError::UnknownReceipt(receipt.as_str().to_owned()))?;
        if pending.receipt != *receipt {
            return Err(JournalError::UnknownReceipt(receipt.as_str().to_owned()));
        }
        if pending.run_id != run_id {
            return Err(JournalError::RunMismatch);
        }
        let pending = state
            .pending
            .take()
            .expect("pending exchange was checked above");
        state
            .completed
            .push(ConversationMessage::Assistant(pending.assistant));
        state
            .completed
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
    run_id: String,
    audit: DemoAudit,
}

impl DemoRecorder {
    pub(crate) fn new(journal: DemoJournal, run_id: String, audit: DemoAudit) -> Arc<Self> {
        Arc::new(Self {
            journal,
            run_id,
            audit,
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
pub(crate) enum JournalError {
    #[error("another tool exchange is already pending")]
    PendingExchangeExists,
    #[error("failed to create exchange receipt")]
    InvalidReceipt(RecordError),
    #[error("unknown pending exchange `{0}`")]
    UnknownReceipt(String),
    #[error("pending exchange belongs to another run")]
    RunMismatch,
}

#[cfg(test)]
mod tests {
    use agent_types::{FinishReason, MessageId, ModelIdentity, ProviderId};

    use super::*;

    fn assistant() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant-1").expect("valid id"),
            model: ModelIdentity::new(ProviderId::new("test").expect("valid provider"), "model"),
            parts: vec![],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    #[tokio::test]
    async fn pending_exchange_is_not_visible_in_snapshot() {
        let journal = DemoJournal::default();
        let recorder = DemoRecorder::new(journal.clone(), "run-1".to_owned(), DemoAudit::default());
        let receipt = recorder
            .begin_tool_exchange(assistant())
            .await
            .expect("begin exchange");

        assert!(journal.has_pending());
        assert!(journal.snapshot().messages.is_empty());

        recorder
            .complete_tool_exchange(&receipt, vec![])
            .await
            .expect("complete exchange");
        assert!(!journal.has_pending());
        assert_eq!(journal.snapshot().messages.len(), 1);
    }
}
