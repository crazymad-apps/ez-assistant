//! Demo 私有的两阶段 Session Journal / ExecutionRecorder。

use std::{collections::HashSet, path::PathBuf};

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, MessageId, ToolMessage, ToolResult,
    ToolResultContent, ToolResultStatus,
};
use tokio::sync::Mutex;

use crate::{
    atomic_json::AtomicJsonWriter,
    session::{PendingToolExchange, SessionRecord, save_session, validate_record},
};

/// completed 消息写入 ConversationSnapshot，pending exchange 只保存为恢复事实。
pub(crate) struct DemoJournal {
    path: PathBuf,
    writer: AtomicJsonWriter,
    state: Mutex<SessionRecord>,
}

impl DemoJournal {
    pub(crate) fn new(path: PathBuf, record: SessionRecord) -> Result<Self, RecordError> {
        validate_record(&record, &record.id).map_err(record_error)?;
        Ok(Self {
            path,
            writer: AtomicJsonWriter::default(),
            state: Mutex::new(record),
        })
    }

    #[cfg(test)]
    fn with_writer(
        path: PathBuf,
        record: SessionRecord,
        writer: AtomicJsonWriter,
    ) -> Result<Self, RecordError> {
        validate_record(&record, &record.id).map_err(record_error)?;
        Ok(Self {
            path,
            writer,
            state: Mutex::new(record),
        })
    }

    pub(crate) async fn record(&self) -> SessionRecord {
        self.state.lock().await.clone()
    }

    /// 进程重启后把遗留 pending 调用结算为 interrupted，恢复可继续投影的规范对话。
    pub(crate) async fn recover_pending_exchange(&self) -> Result<bool, RecordError> {
        let pending = self.state.lock().await.pending_exchange.clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        let receipt = ExchangeReceipt::new(pending.receipt.clone())?;
        let results = tool_call_ids(&pending.assistant)
            .into_iter()
            .enumerate()
            .map(|(index, call_id)| {
                let message_id = format!("recovered_{}_{index}", pending.receipt);
                Ok(ToolMessage {
                    id: MessageId::new(message_id).map_err(|error| RecordError {
                        message: error.to_string(),
                    })?,
                    result: ToolResult {
                        call_id: call_id.clone(),
                        status: ToolResultStatus::Error,
                        content: ToolResultContent::text(
                            "tool execution outcome is unknown because the previous demo process \
                             stopped before the exchange completed"
                                .to_owned(),
                        ),
                        metadata: None,
                    },
                })
            })
            .collect::<Result<Vec<_>, RecordError>>()?;
        self.complete_tool_exchange(&receipt, results).await?;
        Ok(true)
    }

    /// Runtime/Demo 追加非工具消息时同样走一次原子 Session 更新。
    pub(crate) async fn append_message(
        &self,
        message: ConversationMessage,
    ) -> Result<(), RecordError> {
        if matches!(message, ConversationMessage::Tool(_))
            || matches!(
                &message,
                ConversationMessage::Assistant(assistant)
                    if assistant.parts.iter().any(|part| matches!(part, AssistantPart::ToolCall(_)))
            )
        {
            return Err(RecordError {
                message: "tool exchanges must use the two-phase recorder methods".to_owned(),
            });
        }
        let mut state = self.state.lock().await;
        if state.pending_exchange.is_some() {
            return Err(RecordError {
                message: "cannot append a message while a tool exchange is pending".to_owned(),
            });
        }
        let mut candidate = state.clone();
        candidate.conversation.messages.push(message);
        candidate
            .conversation
            .validate_tool_exchange_pairs()
            .map_err(|error| RecordError {
                message: error.to_string(),
            })?;
        validate_record(&candidate, &candidate.id).map_err(record_error)?;
        save_session(&self.path, &candidate, self.writer)
            .await
            .map_err(record_error)?;
        *state = candidate;
        Ok(())
    }
}

impl ExecutionRecorder for DemoJournal {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            let call_ids = tool_call_ids(&assistant);
            if call_ids.is_empty() {
                return Err(RecordError {
                    message: "tool exchange assistant message has no tool calls".to_owned(),
                });
            }
            let mut unique = HashSet::new();
            if call_ids.iter().any(|id| !unique.insert(id.as_str())) {
                return Err(RecordError {
                    message: "tool exchange contains duplicate call ids".to_owned(),
                });
            }

            let mut state = self.state.lock().await;
            if state.pending_exchange.is_some() {
                return Err(RecordError {
                    message: "another tool exchange is already pending".to_owned(),
                });
            }
            let next_exchange_id =
                state
                    .next_exchange_id
                    .checked_add(1)
                    .ok_or_else(|| RecordError {
                        message: "tool exchange sequence is exhausted".to_owned(),
                    })?;
            let receipt = ExchangeReceipt::new(format!("exchange_{:010}", state.next_exchange_id))?;
            let mut candidate = state.clone();
            candidate.next_exchange_id = next_exchange_id;
            candidate.pending_exchange = Some(PendingToolExchange {
                receipt: receipt.as_str().to_owned(),
                assistant,
            });
            validate_record(&candidate, &candidate.id).map_err(record_error)?;
            save_session(&self.path, &candidate, self.writer)
                .await
                .map_err(record_error)?;
            *state = candidate;
            Ok(receipt)
        })
    }

    fn mark_tool_execution_started<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        call_id: &'a agent_types::ToolCallId,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let state = self.state.lock().await;
            let pending = state.pending_exchange.as_ref().ok_or_else(|| RecordError {
                message: "tool execution start has no pending exchange".to_owned(),
            })?;
            let matches = pending.receipt == receipt.as_str()
                && tool_call_ids(&pending.assistant)
                    .into_iter()
                    .any(|candidate| candidate == call_id);
            if matches {
                Ok(())
            } else {
                Err(RecordError {
                    message: "tool execution start does not match pending exchange".to_owned(),
                })
            }
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let pending = state.pending_exchange.as_ref().ok_or_else(|| RecordError {
                message: "tool exchange receipt has no pending exchange".to_owned(),
            })?;
            if pending.receipt != receipt.as_str() {
                return Err(RecordError {
                    message: "tool exchange receipt does not match pending state".to_owned(),
                });
            }
            let expected = tool_call_ids(&pending.assistant);
            let actual = results
                .iter()
                .map(|message| &message.result.call_id)
                .collect::<Vec<_>>();
            if expected != actual {
                return Err(RecordError {
                    message: "tool results do not match pending call ids in order".to_owned(),
                });
            }

            let mut candidate = state.clone();
            let pending = candidate
                .pending_exchange
                .take()
                .expect("pending exchange checked above");
            candidate
                .conversation
                .messages
                .push(ConversationMessage::Assistant(pending.assistant));
            candidate
                .conversation
                .messages
                .extend(results.into_iter().map(ConversationMessage::Tool));
            candidate
                .conversation
                .validate_tool_exchange_pairs()
                .map_err(|error| RecordError {
                    message: error.to_string(),
                })?;
            validate_record(&candidate, &candidate.id).map_err(record_error)?;
            save_session(&self.path, &candidate, self.writer)
                .await
                .map_err(record_error)?;
            *state = candidate;
            Ok(())
        })
    }
}

fn tool_call_ids(assistant: &AssistantMessage) -> Vec<&agent_types::ToolCallId> {
    assistant
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(&call.id),
            _ => None,
        })
        .collect()
}

fn record_error(error: impl std::fmt::Display) -> RecordError {
    RecordError {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use agent_model::SystemPromptSnapshot;
    use agent_types::{
        FinishReason, MessageId, ModelIdentity, PartId, ProviderId, TextPart, ToolCall, ToolCallId,
        ToolName, ToolResult, ToolResultContent, ToolResultStatus,
    };

    use crate::session::{SessionRecord, restore_session, session_path};

    use super::*;

    fn record(id: &str) -> SessionRecord {
        SessionRecord {
            version: 1,
            id: id.to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["frozen".to_owned()]),
            conversation: agent_types::ConversationSnapshot::default(),
            next_exchange_id: 1,
            pending_exchange: None,
        }
    }

    fn assistant(call_ids: &[&str]) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: call_ids
                .iter()
                .map(|id| {
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new(*id).expect("valid call id"),
                        name: ToolName::new("pin_memory").expect("valid tool name"),
                        arguments: serde_json::json!({}),
                    })
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn result(id: &str, message_id: &str) -> ToolMessage {
        ToolMessage {
            id: MessageId::new(message_id).expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new(id).expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("ok".to_owned()),
                metadata: None,
            },
        }
    }

    #[tokio::test]
    async fn journal_persists_pending_then_atomically_projects_completed_exchange() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sessions = directory.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("create sessions directory");
        let path = session_path(&sessions, "journal");
        crate::session::save_session(&path, &record("journal"), AtomicJsonWriter::default())
            .await
            .expect("save initial session");
        let journal = DemoJournal::new(path, record("journal")).expect("create journal");

        let receipt = journal
            .begin_tool_exchange(assistant(&["call_1", "call_2"]))
            .await
            .expect("begin exchange");
        let pending = restore_session(&sessions, "journal")
            .await
            .expect("restore pending session");
        assert!(pending.conversation.messages.is_empty());
        assert!(pending.pending_exchange.is_some());

        journal
            .complete_tool_exchange(
                &receipt,
                vec![result("call_1", "tool_1"), result("call_2", "tool_2")],
            )
            .await
            .expect("complete exchange");
        let completed = restore_session(&sessions, "journal")
            .await
            .expect("restore completed session");
        assert!(completed.pending_exchange.is_none());
        assert_eq!(completed.conversation.messages.len(), 3);
        completed
            .conversation
            .validate_tool_exchange_pairs()
            .expect("completed pairs");
    }

    #[tokio::test]
    async fn failed_complete_preserves_pending_and_never_writes_partial_results() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sessions = directory.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("create sessions directory");
        let path = session_path(&sessions, "failure");
        let initial = record("failure");
        let good = DemoJournal::new(path.clone(), initial.clone()).expect("create journal");
        crate::session::save_session(&path, &initial, AtomicJsonWriter::default())
            .await
            .expect("save initial session");
        let receipt = good
            .begin_tool_exchange(assistant(&["call_1"]))
            .await
            .expect("begin exchange");
        let pending = good.record().await;
        drop(good);

        let failing =
            DemoJournal::with_writer(path, pending, AtomicJsonWriter::failing_before_persist())
                .expect("create failing journal");
        assert!(
            failing
                .complete_tool_exchange(&receipt, vec![result("call_1", "tool_1")])
                .await
                .is_err()
        );
        let restored = restore_session(&sessions, "failure")
            .await
            .expect("restore failed completion");
        assert!(restored.pending_exchange.is_some());
        assert!(restored.conversation.messages.is_empty());
    }

    #[tokio::test]
    async fn journal_rejects_out_of_order_results_and_routes_plain_messages_separately() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sessions = directory.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("create sessions directory");
        let path = session_path(&sessions, "order");
        let initial = record("order");
        crate::session::save_session(&path, &initial, AtomicJsonWriter::default())
            .await
            .expect("save initial session");
        let journal = DemoJournal::new(path, initial).expect("create journal");
        journal
            .append_message(ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new("assistant_text").expect("valid message id"),
                model: ModelIdentity::new(
                    ProviderId::new("test").expect("valid provider id"),
                    "test-model",
                ),
                parts: vec![AssistantPart::Text(TextPart {
                    id: PartId::new("text_1").expect("valid part id"),
                    text: "hello".to_owned(),
                })],
                finish_reason: FinishReason::Stop,
                usage: None,
            }))
            .await
            .expect("append plain assistant");
        let receipt = journal
            .begin_tool_exchange(assistant(&["call_1", "call_2"]))
            .await
            .expect("begin exchange");
        assert!(
            journal
                .complete_tool_exchange(
                    &receipt,
                    vec![result("call_2", "tool_2"), result("call_1", "tool_1")],
                )
                .await
                .is_err()
        );
        assert!(journal.record().await.pending_exchange.is_some());
    }

    #[tokio::test]
    async fn restart_recovery_completes_pending_calls_as_errors() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sessions = directory.path().join("sessions");
        tokio::fs::create_dir_all(&sessions)
            .await
            .expect("create sessions directory");
        let path = session_path(&sessions, "recovery");
        let initial = record("recovery");
        crate::session::save_session(&path, &initial, AtomicJsonWriter::default())
            .await
            .expect("save initial session");
        let first = DemoJournal::new(path.clone(), initial).expect("create journal");
        first
            .begin_tool_exchange(assistant(&["call_1"]))
            .await
            .expect("begin exchange");
        drop(first);

        let restored = restore_session(&sessions, "recovery")
            .await
            .expect("restore pending session");
        let rebuilt = DemoJournal::new(path, restored).expect("rebuild journal");
        assert!(rebuilt.recover_pending_exchange().await.expect("recover"));
        let record = rebuilt.record().await;
        assert!(record.pending_exchange.is_none());
        assert_eq!(record.conversation.messages.len(), 2);
        let ConversationMessage::Tool(tool) = &record.conversation.messages[1] else {
            panic!("second message must be recovered tool result");
        };
        assert_eq!(tool.result.status, ToolResultStatus::Error);
        record
            .conversation
            .validate_tool_exchange_pairs()
            .expect("recovered pairs");
    }
}
