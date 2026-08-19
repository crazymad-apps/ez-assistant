//! 单 Session 的内存规范 Conversation 与 pending tool exchange。

use agent_core::ExchangeReceipt;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot,
    ConversationValidationError, ToolMessage,
};
use assistant_protocol::RunId;
use thiserror::Error;

struct PendingExchange {
    run_id: RunId,
    receipt: ExchangeReceipt,
    assistant: AssistantMessage,
}

/// 内存 Journal 的结构不变量错误。
#[derive(Debug, Error)]
pub(crate) enum JournalError {
    /// 上一个工具交换尚未完成。
    #[error("journal already has a pending tool exchange")]
    PendingExchangeExists,
    /// 当前没有可完成的工具交换。
    #[error("journal has no pending tool exchange")]
    NoPendingExchange,
    /// Recorder 所属 Run 与 pending exchange 不一致。
    #[error("journal run id does not match the pending exchange")]
    RunMismatch,
    /// complete 使用的 receipt 与当前 pending 不一致。
    #[error("journal receipt does not match the pending exchange")]
    ExchangeMismatch,
    /// Journal 内部的交换序号已经耗尽。
    #[cfg(test)]
    #[error("journal exchange id sequence is exhausted")]
    ExchangeIdExhausted,
    /// begin 收到的 AssistantMessage 没有 Tool Call。
    #[error("pending assistant message must contain at least one tool call")]
    AssistantHasNoToolCalls,
    /// 新提交会破坏 Tool Call/Result 的规范配对或顺序。
    #[error("completed conversation is invalid")]
    InvalidConversation {
        /// `agent-types` 提供的具体规范校验错误。
        #[source]
        source: ConversationValidationError,
    },
}

/// 不持久化的单 Session Conversation Journal。
pub(crate) struct InMemoryJournal {
    completed: Vec<ConversationMessage>,
    pending: Option<PendingExchange>,
    #[cfg(test)]
    next_exchange: u64,
}

impl InMemoryJournal {
    pub(crate) fn new() -> Self {
        Self {
            completed: Vec::new(),
            pending: None,
            #[cfg(test)]
            next_exchange: 1,
        }
    }

    pub(crate) fn from_snapshot(snapshot: ConversationSnapshot) -> Result<Self, JournalError> {
        validate(&snapshot.messages)?;
        Ok(Self {
            completed: snapshot.messages,
            pending: None,
            #[cfg(test)]
            next_exchange: 1,
        })
    }

    pub(crate) fn message_count(&self) -> usize {
        self.completed.len()
    }

    pub(crate) fn snapshot(&self) -> ConversationSnapshot {
        ConversationSnapshot::new(self.completed.clone())
    }

    pub(crate) fn replace_completed(
        &mut self,
        snapshot: ConversationSnapshot,
    ) -> Result<(), JournalError> {
        if self.pending.is_some() {
            return Err(JournalError::PendingExchangeExists);
        }
        validate(&snapshot.messages)?;
        self.completed = snapshot.messages;
        Ok(())
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn append_completed(
        &mut self,
        message: ConversationMessage,
    ) -> Result<(), JournalError> {
        if self.pending.is_some() {
            return Err(JournalError::PendingExchangeExists);
        }
        let mut candidate = self.completed.clone();
        candidate.push(message);
        validate(&candidate)?;
        self.completed = candidate;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn begin_tool_exchange(
        &mut self,
        run_id: &RunId,
        assistant: AssistantMessage,
    ) -> Result<ExchangeReceipt, JournalError> {
        self.validate_tool_exchange_begin(&assistant)?;
        let receipt = ExchangeReceipt::new(format!("exchange-{}", self.next_exchange))
            .map_err(|_| JournalError::ExchangeIdExhausted)?;
        self.next_exchange = self
            .next_exchange
            .checked_add(1)
            .ok_or(JournalError::ExchangeIdExhausted)?;
        self.begin_tool_exchange_with_receipt(run_id, receipt.clone(), assistant)?;
        Ok(receipt)
    }

    /// 以 Runtime 已可靠持久化的 receipt 建立内存 pending 投影。
    pub(crate) fn begin_tool_exchange_with_receipt(
        &mut self,
        run_id: &RunId,
        receipt: ExchangeReceipt,
        assistant: AssistantMessage,
    ) -> Result<(), JournalError> {
        self.validate_tool_exchange_begin(&assistant)?;
        self.pending = Some(PendingExchange {
            run_id: run_id.clone(),
            receipt,
            assistant,
        });
        Ok(())
    }

    /// 在 Store await 前只检查内存前置条件，不产生规范或 pending 状态变化。
    pub(crate) fn validate_tool_exchange_begin(
        &self,
        assistant: &AssistantMessage,
    ) -> Result<(), JournalError> {
        if self.pending.is_some() {
            return Err(JournalError::PendingExchangeExists);
        }
        if !assistant
            .parts
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        {
            return Err(JournalError::AssistantHasNoToolCalls);
        }
        Ok(())
    }

    /// 构造 complete 将提交的完整批次并验证配对，但不提前清除 pending。
    pub(crate) fn tool_exchange_batch(
        &self,
        run_id: &RunId,
        receipt: &ExchangeReceipt,
        results: &[ToolMessage],
    ) -> Result<Vec<ConversationMessage>, JournalError> {
        let pending = self.pending(run_id, receipt)?;
        let mut batch = vec![ConversationMessage::Assistant(pending.assistant.clone())];
        batch.extend(results.iter().cloned().map(ConversationMessage::Tool));
        let mut candidate = self.completed.clone();
        candidate.extend(batch.iter().cloned());
        validate(&candidate)?;
        Ok(batch)
    }

    pub(crate) fn complete_tool_exchange(
        &mut self,
        run_id: &RunId,
        receipt: &ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> Result<(), JournalError> {
        let batch = self.tool_exchange_batch(run_id, receipt, &results)?;
        self.completed.extend(batch);
        self.pending = None;
        Ok(())
    }

    fn pending(
        &self,
        run_id: &RunId,
        receipt: &ExchangeReceipt,
    ) -> Result<&PendingExchange, JournalError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(JournalError::NoPendingExchange)?;
        if &pending.run_id != run_id {
            return Err(JournalError::RunMismatch);
        }
        if &pending.receipt != receipt {
            return Err(JournalError::ExchangeMismatch);
        }
        Ok(pending)
    }
}

fn validate(messages: &[ConversationMessage]) -> Result<(), JournalError> {
    ConversationSnapshot::new(messages.to_vec())
        .validate_tool_exchange_pairs()
        .map_err(|source| JournalError::InvalidConversation { source })
}

#[cfg(test)]
mod tests {
    use agent_types::{
        FinishReason, MessageId, ModelIdentity, ProviderId, ToolCall, ToolCallId, ToolName,
        ToolResult, ToolResultContent, ToolResultStatus,
    };
    use serde_json::json;

    use super::*;

    fn make_run_id(value: &str) -> RunId {
        RunId::new(value).expect("run id")
    }

    fn assistant_with_calls(call_ids: &[&str]) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant-1").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: call_ids
                .iter()
                .map(|call_id| {
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new(*call_id).expect("tool call id"),
                        name: ToolName::new("echo_text").expect("tool name"),
                        arguments: json!({"text": call_id}),
                    })
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn result(message_id: &str, call_id: &str) -> ToolMessage {
        ToolMessage {
            id: MessageId::new(message_id).expect("message id"),
            result: ToolResult {
                call_id: ToolCallId::new(call_id).expect("tool call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text(format!("result for {call_id}")),
                metadata: None,
            },
        }
    }

    #[test]
    fn pending_exchange_is_hidden_until_the_complete_batch_is_valid() {
        let mut journal = InMemoryJournal::new();
        let run_id = make_run_id("r_test");
        let id = journal
            .begin_tool_exchange(&run_id, assistant_with_calls(&["call-1", "call-2"]))
            .expect("begin succeeds");

        assert!(journal.snapshot().messages.is_empty());
        assert!(matches!(
            journal.complete_tool_exchange(
                &run_id,
                &id,
                vec![result("tool-2", "call-2"), result("tool-1", "call-1")]
            ),
            Err(JournalError::InvalidConversation { .. })
        ));
        assert!(journal.snapshot().messages.is_empty());

        journal
            .complete_tool_exchange(
                &run_id,
                &id,
                vec![result("tool-1", "call-1"), result("tool-2", "call-2")],
            )
            .expect("ordered batch completes");
        let snapshot = journal.snapshot();
        assert_eq!(snapshot.messages.len(), 3);
        snapshot
            .validate_tool_exchange_pairs()
            .expect("completed snapshot remains valid");
    }

    #[test]
    fn reentry_mismatch_and_duplicate_complete_preserve_state() {
        let mut journal = InMemoryJournal::new();
        let run_id = make_run_id("r_test");
        let id = journal
            .begin_tool_exchange(&run_id, assistant_with_calls(&["call-1"]))
            .expect("begin succeeds");
        assert!(matches!(
            journal.begin_tool_exchange(&run_id, assistant_with_calls(&["call-2"])),
            Err(JournalError::PendingExchangeExists)
        ));
        let wrong_receipt = ExchangeReceipt::new("wrong").expect("receipt");
        assert!(matches!(
            journal.complete_tool_exchange(&run_id, &wrong_receipt, vec![]),
            Err(JournalError::ExchangeMismatch)
        ));
        assert!(matches!(
            journal.complete_tool_exchange(&make_run_id("r_other"), &id, vec![]),
            Err(JournalError::RunMismatch)
        ));

        journal
            .complete_tool_exchange(&run_id, &id, vec![result("tool-1", "call-1")])
            .expect("original pending remains intact");
        assert!(matches!(
            journal.complete_tool_exchange(&run_id, &id, vec![]),
            Err(JournalError::NoPendingExchange)
        ));
    }

    #[test]
    fn begin_requires_tool_calls_and_append_cannot_cross_pending() {
        let mut journal = InMemoryJournal::new();
        let run_id = make_run_id("r_test");
        assert!(matches!(
            journal.begin_tool_exchange(&run_id, assistant_with_calls(&[])),
            Err(JournalError::AssistantHasNoToolCalls)
        ));

        let _id = journal
            .begin_tool_exchange(&run_id, assistant_with_calls(&["call-1"]))
            .expect("begin succeeds");
        assert!(matches!(
            journal.append_completed(ConversationMessage::Assistant(assistant_with_calls(&[]))),
            Err(JournalError::PendingExchangeExists)
        ));
    }
}
