//! In-memory Conversation Journal and two-phase execution recorder.

use std::{
    future::ready,
    sync::{Arc, Mutex, MutexGuard},
};

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{
    AssistantMessage, ConversationMessage, ConversationSnapshot, IdentifierError, ToolMessage,
    UserMessage,
};
use thiserror::Error;

use crate::runtime::HarnessRunId;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum JournalError {
    #[error("in-memory journal lock is poisoned")]
    Poisoned,
    #[error("failed to create an exchange receipt")]
    InvalidReceipt(#[source] RecordError),
    #[error("unknown pending exchange `{0}`")]
    UnknownReceipt(String),
    #[error("pending exchange `{receipt}` does not belong to run `{run_id}`")]
    ReceiptRunMismatch { receipt: String, run_id: String },
    #[error("pending tool exchange blocks a new run; inspect state and reset")]
    PendingBlocksRun,
    #[error("failed to create a conversation identifier")]
    InvalidIdentifier(#[source] IdentifierError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingSummary {
    pub(crate) receipt: String,
    pub(crate) assistant_message_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct JournalSnapshot {
    pub(crate) conversation: ConversationSnapshot,
    pub(crate) pending: Vec<PendingSummary>,
}

struct JournalState {
    messages: Vec<ConversationMessage>,
    pending: Vec<PendingExchange>,
    next_exchange: u64,
}

struct PendingExchange {
    receipt: ExchangeReceipt,
    run_id: HarnessRunId,
    assistant: AssistantMessage,
}

pub(crate) struct HarnessJournal {
    state: Mutex<JournalState>,
}

impl HarnessJournal {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(JournalState {
                messages: Vec::new(),
                pending: Vec::new(),
                next_exchange: 1,
            }),
        })
    }

    pub(crate) fn snapshot(&self) -> Result<JournalSnapshot, JournalError> {
        let state = self.lock()?;
        Ok(JournalSnapshot {
            conversation: ConversationSnapshot::new(state.messages.clone()),
            pending: state
                .pending
                .iter()
                .map(|exchange| PendingSummary {
                    receipt: exchange.receipt.as_str().to_owned(),
                    assistant_message_id: exchange.assistant.id.to_string(),
                })
                .collect(),
        })
    }

    pub(crate) fn has_pending(&self) -> Result<bool, JournalError> {
        Ok(!self.lock()?.pending.is_empty())
    }

    pub(crate) fn append_user(&self, message: UserMessage) -> Result<(), JournalError> {
        self.lock()?
            .messages
            .push(ConversationMessage::User(message));
        Ok(())
    }

    pub(crate) fn append_assistant(&self, message: AssistantMessage) -> Result<(), JournalError> {
        self.lock()?
            .messages
            .push(ConversationMessage::Assistant(message));
        Ok(())
    }

    pub(crate) fn reset(&self) -> Result<(), JournalError> {
        let mut state = self.lock()?;
        state.messages.clear();
        state.pending.clear();
        Ok(())
    }

    fn begin(
        &self,
        run_id: &HarnessRunId,
        assistant: AssistantMessage,
    ) -> Result<ExchangeReceipt, JournalError> {
        let mut state = self.lock()?;
        let receipt_value = format!("{run_id}_exchange_{}", state.next_exchange);
        let receipt = ExchangeReceipt::new(receipt_value).map_err(JournalError::InvalidReceipt)?;
        state.next_exchange += 1;
        state.pending.push(PendingExchange {
            receipt: receipt.clone(),
            run_id: run_id.clone(),
            assistant,
        });
        Ok(receipt)
    }

    fn complete(
        &self,
        run_id: &HarnessRunId,
        receipt: &ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> Result<(), JournalError> {
        let mut state = self.lock()?;
        let index = state
            .pending
            .iter()
            .position(|exchange| exchange.receipt == *receipt)
            .ok_or_else(|| JournalError::UnknownReceipt(receipt.as_str().to_owned()))?;
        if state.pending[index].run_id != *run_id {
            return Err(JournalError::ReceiptRunMismatch {
                receipt: receipt.as_str().to_owned(),
                run_id: run_id.to_string(),
            });
        }

        let mut completed = Vec::with_capacity(results.len() + 1);
        completed.push(ConversationMessage::Assistant(
            state.pending[index].assistant.clone(),
        ));
        completed.extend(results.into_iter().map(ConversationMessage::Tool));
        state.messages.extend(completed);
        state.pending.remove(index);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, JournalState>, JournalError> {
        self.state.lock().map_err(|_| JournalError::Poisoned)
    }
}

pub(crate) struct HarnessRecorder {
    journal: Arc<HarnessJournal>,
    run_id: HarnessRunId,
}

impl HarnessRecorder {
    pub(crate) fn new(journal: Arc<HarnessJournal>, run_id: HarnessRunId) -> Arc<Self> {
        Arc::new(Self { journal, run_id })
    }
}

impl ExecutionRecorder for HarnessRecorder {
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

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use agent_core::ExecutionRecorder;
    use agent_types::{
        FinishReason, MessageId, ModelIdentity, ProviderId, ToolCallId, ToolResult,
        ToolResultContent, ToolResultStatus,
    };

    use super::*;

    fn run_id() -> HarnessRunId {
        HarnessRunId::from_sequence(7)
    }

    fn assistant(id: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("valid provider id"),
                "scripted-model",
            ),
            parts: Vec::new(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn tool_message(id: &str) -> ToolMessage {
        ToolMessage {
            id: MessageId::new(format!("tool_message_{id}")).expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new(format!("call_{id}")).expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text(format!("result {id}")),
            },
        }
    }

    #[tokio::test]
    async fn pending_is_hidden_until_the_whole_exchange_completes() {
        let journal = HarnessJournal::new();
        let recorder = HarnessRecorder::new(Arc::clone(&journal), run_id());
        let receipt = recorder
            .begin_tool_exchange(assistant("assistant_1"))
            .await
            .expect("begin succeeds");

        let pending = journal.snapshot().expect("snapshot");
        assert!(pending.conversation.messages.is_empty());
        assert_eq!(
            pending.pending,
            vec![PendingSummary {
                receipt: "run_7_exchange_1".to_owned(),
                assistant_message_id: "assistant_1".to_owned(),
            }]
        );

        recorder
            .complete_tool_exchange(&receipt, vec![tool_message("1"), tool_message("2")])
            .await
            .expect("complete succeeds");
        let completed = journal.snapshot().expect("snapshot");
        assert!(completed.pending.is_empty());
        assert!(matches!(
            completed.conversation.messages.as_slice(),
            [
                ConversationMessage::Assistant(_),
                ConversationMessage::Tool(_),
                ConversationMessage::Tool(_)
            ]
        ));
    }

    #[tokio::test]
    async fn unknown_receipt_is_controlled_and_keeps_pending_unchanged() {
        let journal = HarnessJournal::new();
        let recorder = HarnessRecorder::new(Arc::clone(&journal), run_id());
        recorder
            .begin_tool_exchange(assistant("assistant_1"))
            .await
            .expect("begin succeeds");
        let unknown = ExchangeReceipt::new("missing").expect("valid receipt");

        let error = recorder
            .complete_tool_exchange(&unknown, vec![tool_message("1")])
            .await
            .expect_err("unknown receipt must fail");
        assert!(error.message.contains("unknown pending exchange"));
        let snapshot = journal.snapshot().expect("snapshot");
        assert!(snapshot.conversation.messages.is_empty());
        assert_eq!(snapshot.pending.len(), 1);
    }

    #[tokio::test]
    async fn receipt_cannot_be_completed_by_another_run_recorder() {
        let journal = HarnessJournal::new();
        let owner = HarnessRecorder::new(Arc::clone(&journal), run_id());
        let receipt = owner
            .begin_tool_exchange(assistant("assistant_1"))
            .await
            .expect("begin succeeds");
        let other = HarnessRecorder::new(Arc::clone(&journal), HarnessRunId::from_sequence(8));

        let error = other
            .complete_tool_exchange(&receipt, vec![tool_message("1")])
            .await
            .expect_err("cross-run completion must fail");
        assert!(error.message.contains("does not belong to run `run_8`"));
        let snapshot = journal.snapshot().expect("snapshot");
        assert!(snapshot.conversation.messages.is_empty());
        assert_eq!(snapshot.pending.len(), 1);
    }

    #[test]
    fn poisoned_lock_returns_an_error_instead_of_panicking() {
        let journal = HarnessJournal::new();
        let journal_for_panic = Arc::clone(&journal);
        let result = catch_unwind(AssertUnwindSafe(move || {
            let _guard = journal_for_panic.state.lock().expect("lock before poison");
            panic!("poison journal for test");
        }));
        assert!(result.is_err());
        assert_eq!(journal.snapshot(), Err(JournalError::Poisoned));
    }
}
