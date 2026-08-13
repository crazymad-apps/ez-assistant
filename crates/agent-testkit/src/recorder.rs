//! 内存 [`ExecutionRecorder`]：pending/completed 两阶段 tool exchange，可注入
//! “第 N 次 Recorder 调用失败”。
//!
//! begin/complete 的每次尝试都写入共享 [`OrderLog`](crate::OrderLog)。complete
//! 成功时一次性把 Assistant + 完整 ToolMessage 批次加入规范投影视图；失败时
//! 不写入部分结果，pending exchange 保持可恢复。

use std::sync::Mutex;

use agent_core::{
    ConversationDelta, ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture,
};
use agent_types::{AssistantMessage, AssistantPart, ToolCallId, ToolMessage};

use crate::OrderLog;
use crate::order::LogEntry;

/// 内存 Recorder Fake。
pub struct InMemoryRecorder {
    state: Mutex<RecorderState>,
    /// 第 N 次 Recorder 调用（begin/complete 合计，从 1 开始）注入失败。
    fail_at: Option<u64>,
    fail_start: bool,
    log: OrderLog,
}

#[derive(Default)]
struct RecorderState {
    /// completed exchange 展平后的规范投影视图。
    deltas: Vec<ConversationDelta>,
    pending: Vec<PendingExchange>,
    started: Vec<(ExchangeReceipt, ToolCallId)>,
    attempts: u64,
    next_exchange: u64,
}

#[derive(Clone)]
struct PendingExchange {
    receipt: ExchangeReceipt,
    assistant: AssistantMessage,
}

impl InMemoryRecorder {
    /// 创建永不失败的 Recorder。
    pub fn new(log: OrderLog) -> Self {
        Self {
            state: Mutex::new(RecorderState::default()),
            fail_at: None,
            fail_start: false,
            log,
        }
    }

    /// 创建在第 `call` 次 begin/complete 调用（从 1 开始）注入失败的 Recorder。
    pub fn failing_at(call: u64, log: OrderLog) -> Self {
        assert!(call > 0, "fail_at counts recorder calls from 1");
        Self {
            fail_at: Some(call),
            ..Self::new(log)
        }
    }

    /// 创建一个在 started 可靠记录点失败的 Recorder。
    pub fn failing_start(log: OrderLog) -> Self {
        Self {
            fail_start: true,
            ..Self::new(log)
        }
    }

    /// completed exchange 展平后的规范增量；pending 不会出现在此视图中。
    pub fn deltas(&self) -> Vec<ConversationDelta> {
        self.state
            .lock()
            .expect("recorder mutex poisoned")
            .deltas
            .clone()
    }

    /// 当前 pending exchange 的有序快照（恢复断言用）。
    pub fn pending_exchanges(&self) -> Vec<(ExchangeReceipt, AssistantMessage)> {
        self.state
            .lock()
            .expect("recorder mutex poisoned")
            .pending
            .iter()
            .map(|pending| (pending.receipt.clone(), pending.assistant.clone()))
            .collect()
    }

    /// 已在副作用前确认 started 的 receipt/call 快照。
    pub fn started_calls(&self) -> Vec<(ExchangeReceipt, ToolCallId)> {
        self.state
            .lock()
            .expect("recorder mutex poisoned")
            .started
            .clone()
    }

    fn next_attempt(state: &mut RecorderState, fail_at: Option<u64>) -> Result<(), RecordError> {
        state.attempts += 1;
        if fail_at == Some(state.attempts) {
            return Err(RecordError {
                message: format!("injected record failure at call {}", state.attempts),
            });
        }
        Ok(())
    }
}

impl ExecutionRecorder for InMemoryRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            self.log.push(LogEntry::RecordAssistant);
            let mut state = self.state.lock().expect("recorder mutex poisoned");
            Self::next_attempt(&mut state, self.fail_at)?;
            state.next_exchange += 1;
            let receipt = ExchangeReceipt::new(format!("exchange_{}", state.next_exchange))?;
            state.pending.push(PendingExchange {
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
            if self.fail_start {
                return Err(RecordError {
                    message: "injected tool start record failure".to_owned(),
                });
            }
            let mut state = self.state.lock().expect("recorder mutex poisoned");
            let pending = state
                .pending
                .iter()
                .find(|pending| pending.receipt == *receipt)
                .ok_or_else(|| RecordError {
                    message: format!("unknown pending exchange `{}`", receipt.as_str()),
                })?;
            let contains_call =
                pending.assistant.parts.iter().any(
                    |part| matches!(part, AssistantPart::ToolCall(call) if call.id == *call_id),
                );
            if !contains_call {
                return Err(RecordError {
                    message: "tool call does not belong to pending exchange".to_owned(),
                });
            }
            if !state
                .started
                .iter()
                .any(|started| started.0 == *receipt && started.1 == *call_id)
            {
                state.started.push((receipt.clone(), call_id.clone()));
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
            self.log.push(LogEntry::RecordTool);
            let mut state = self.state.lock().expect("recorder mutex poisoned");
            Self::next_attempt(&mut state, self.fail_at)?;
            let Some(index) = state
                .pending
                .iter()
                .position(|pending| pending.receipt == *receipt)
            else {
                return Err(RecordError {
                    message: format!("unknown pending exchange `{}`", receipt.as_str()),
                });
            };
            let pending = state.pending.remove(index);
            let mut completed = Vec::with_capacity(results.len() + 1);
            completed.push(ConversationDelta::Assistant(pending.assistant));
            completed.extend(results.into_iter().map(ConversationDelta::Tool));
            state.deltas.extend(completed);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, ToolCallId,
        ToolMessage, ToolResult, ToolResultContent, ToolResultStatus,
    };

    use super::*;

    fn assistant_message() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn tool_message() -> ToolMessage {
        ToolMessage {
            id: MessageId::new("toolmsg_1").expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call_1").expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("ok".to_owned()),
            },
        }
    }

    #[tokio::test]
    async fn completed_exchange_becomes_visible_atomically() {
        let log = OrderLog::new();
        let recorder = InMemoryRecorder::new(log.clone());
        let receipt = recorder
            .begin_tool_exchange(assistant_message())
            .await
            .expect("begin exchange");
        assert!(recorder.deltas().is_empty());
        assert_eq!(recorder.pending_exchanges().len(), 1);

        recorder
            .complete_tool_exchange(&receipt, vec![tool_message()])
            .await
            .expect("complete exchange");
        assert_eq!(
            recorder.deltas(),
            vec![
                ConversationDelta::Assistant(assistant_message()),
                ConversationDelta::Tool(tool_message()),
            ]
        );
        assert!(recorder.pending_exchanges().is_empty());
        assert_eq!(
            log.entries(),
            vec![LogEntry::RecordAssistant, LogEntry::RecordTool]
        );
    }

    #[tokio::test]
    async fn failed_complete_keeps_pending_without_partial_projection() {
        let log = OrderLog::new();
        let recorder = InMemoryRecorder::failing_at(2, log.clone());
        let second_tool_message = ToolMessage {
            id: MessageId::new("toolmsg_2").expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call_2").expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("also ok".to_owned()),
            },
        };
        let results = vec![tool_message(), second_tool_message.clone()];
        let receipt = recorder
            .begin_tool_exchange(assistant_message())
            .await
            .expect("begin succeeds");
        let error = recorder
            .complete_tool_exchange(&receipt, results.clone())
            .await
            .expect_err("complete is the injected failure");
        assert!(error.message.contains("at call 2"));
        assert!(recorder.deltas().is_empty());
        assert_eq!(recorder.pending_exchanges().len(), 1);

        // Runtime 恢复可用同一 receipt 原子完成 pending exchange。
        recorder
            .complete_tool_exchange(&receipt, results)
            .await
            .expect("recovery complete succeeds");
        assert_eq!(
            recorder.deltas(),
            vec![
                ConversationDelta::Assistant(assistant_message()),
                ConversationDelta::Tool(tool_message()),
                ConversationDelta::Tool(second_tool_message),
            ]
        );
        assert!(recorder.pending_exchanges().is_empty());
        assert_eq!(
            log.entries(),
            vec![
                LogEntry::RecordAssistant,
                LogEntry::RecordTool,
                LogEntry::RecordTool,
            ]
        );
    }
}
