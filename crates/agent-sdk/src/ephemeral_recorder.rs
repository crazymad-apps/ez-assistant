use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{AssistantMessage, AssistantPart, ToolCallId, ToolMessage};

/// 一个执行内尚未结算的工具交换。
struct PendingExchange {
    receipt: ExchangeReceipt,
    assistant: AssistantMessage,
}

/// `start_ephemeral` 私有使用的单 pending、不可恢复 Recorder。
pub(crate) struct EphemeralExecutionRecorder {
    next_receipt: AtomicU64,
    pending: Mutex<Option<PendingExchange>>,
}

impl EphemeralExecutionRecorder {
    pub(crate) fn new() -> Self {
        Self {
            next_receipt: AtomicU64::new(0),
            pending: Mutex::new(None),
        }
    }

    fn lock_pending(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<PendingExchange>>, RecordError> {
        self.pending.lock().map_err(|_| RecordError {
            message: "ephemeral recorder state is unavailable".to_owned(),
        })
    }
}

impl ExecutionRecorder for EphemeralExecutionRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            let mut pending = self.lock_pending()?;
            if pending.is_some() {
                return Err(RecordError {
                    message: "ephemeral recorder already has a pending exchange".to_owned(),
                });
            }
            let sequence = self.next_receipt.fetch_add(1, Ordering::Relaxed) + 1;
            let receipt = ExchangeReceipt::new(format!("ephemeral_exchange_{sequence}"))?;
            *pending = Some(PendingExchange {
                receipt: receipt.clone(),
                assistant,
            });
            Ok(receipt)
        })
    }

    fn mark_tool_execution_started<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        call_id: &'a ToolCallId,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let pending = self.lock_pending()?;
            let Some(exchange) = pending.as_ref() else {
                return Err(RecordError {
                    message: "ephemeral recorder has no pending exchange".to_owned(),
                });
            };
            if exchange.receipt != *receipt
                || !exchange.assistant.parts.iter().any(
                    |part| matches!(part, AssistantPart::ToolCall(call) if call.id == *call_id),
                )
            {
                return Err(RecordError {
                    message: "tool execution start does not match pending exchange".to_owned(),
                });
            }
            Ok(())
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let mut pending = self.lock_pending()?;
            let Some(exchange) = pending.as_ref() else {
                return Err(RecordError {
                    message: "ephemeral recorder has no pending exchange".to_owned(),
                });
            };
            if exchange.receipt != *receipt {
                return Err(RecordError {
                    message: "ephemeral recorder receipt does not match pending exchange"
                        .to_owned(),
                });
            }

            let completed = pending.take().ok_or_else(|| RecordError {
                message: "ephemeral recorder pending exchange disappeared".to_owned(),
            })?;
            // 临时路径只校验两阶段协议；完整消息和结果在结算后有意丢弃。
            drop((completed.assistant, results));
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId};

    use super::*;

    fn assistant(id: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("valid provider id"),
                "fixture-model",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    #[tokio::test]
    async fn rejects_pending_reentry_and_accepts_next_exchange_after_complete() {
        let recorder = EphemeralExecutionRecorder::new();
        let first = recorder
            .begin_tool_exchange(assistant("message_1"))
            .await
            .expect("first begin succeeds");
        assert!(
            recorder
                .begin_tool_exchange(assistant("message_2"))
                .await
                .expect_err("pending reentry must fail")
                .message
                .contains("already has a pending")
        );
        recorder
            .complete_tool_exchange(&first, vec![])
            .await
            .expect("matching complete succeeds");
        let second = recorder
            .begin_tool_exchange(assistant("message_3"))
            .await
            .expect("begin after complete succeeds");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn receipt_mismatch_keeps_pending_exchange() {
        let recorder = EphemeralExecutionRecorder::new();
        let receipt = recorder
            .begin_tool_exchange(assistant("message_1"))
            .await
            .expect("begin succeeds");
        let other = ExchangeReceipt::new("other_exchange").expect("valid receipt");
        assert!(
            recorder
                .complete_tool_exchange(&other, vec![])
                .await
                .expect_err("mismatched receipt must fail")
                .message
                .contains("does not match")
        );
        recorder
            .complete_tool_exchange(&receipt, vec![])
            .await
            .expect("original pending remains recoverable in process");
    }
}
