//! 工具副作用前的临时落账、完整批次提交与启动恢复。
//!
//! `pending_tool_exchanges` 不是第二份 Conversation。begun 只证明 Tool Call 已在
//! 副作用前可靠保存；ready 额外保存完整结果。只有 staged append 把 Assistant 与
//! 全部 Tool Result 写入正文并完成结构化提交后，pending 行才会被删除。

use std::collections::HashSet;

use agent_core::ExchangeReceipt;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
    ToolMessage, ToolResult, ToolResultContent, ToolResultStatus,
};
use assistant_protocol::{RunId, SessionId};
use assistant_runtime::{CompletedToolExchange, PendingToolExchange};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, append_effect::AppendPurpose, conflict, database_write_error,
    internal_error, invalid_data, invalid_data_with_source, recovery::AppendRequest,
    run_state::system_time_ms,
};

const UNKNOWN_OUTCOME_TEXT: &str = "runtime restarted; tool execution outcome is unknown";

struct StoredPendingExchange {
    receipt: ExchangeReceipt,
    session_id: SessionId,
    run_id: RunId,
    assistant: AssistantMessage,
    results: Option<Vec<ToolMessage>>,
    state: PendingState,
}

#[derive(Clone, Copy)]
enum PendingState {
    Begun,
    Ready,
}

impl StorageEngine {
    /// 工具尚未执行时写入 begun；事务提交成功前 Core 不会进入授权或执行阶段。
    pub(super) fn begin_tool_exchange(
        &mut self,
        pending: PendingToolExchange,
    ) -> StorageResult<()> {
        validate_assistant_calls(&pending.assistant)?;
        let status = self
            .connection
            .query_row(
                "SELECT status FROM runs WHERE run_id = ?1 AND session_id = ?2",
                params![pending.run_id.as_str(), pending.session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("tool exchange run could not be queried", source))?
            .ok_or_else(|| conflict("tool exchange run does not exist"))?;
        if status != "running" {
            return Err(conflict("run cannot begin a tool exchange"));
        }
        let assistant_json = serde_json::to_string(&pending.assistant).map_err(|source| {
            internal_error("pending assistant message could not be encoded", source)
        })?;
        self.connection
            .execute(
                "INSERT INTO pending_tool_exchanges (
                    receipt_id, session_id, run_id, assistant_json, results_json, state, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, NULL, 'begun', ?5)",
                params![
                    pending.receipt.as_str(),
                    pending.session_id.as_str(),
                    pending.run_id.as_str(),
                    assistant_json,
                    pending.created_at_ms,
                ],
            )
            .map_err(|source| {
                database_write_error("pending tool exchange could not be created", source)
            })?;
        Ok(())
    }

    /// 先把结果可靠提升为 ready，再复用 staged append 完成跨 SQLite/正文提交。
    pub(super) fn complete_tool_exchange(
        &mut self,
        completed: CompletedToolExchange,
    ) -> StorageResult<()> {
        let pending = self.load_pending_exchange(completed.receipt.as_str())?;
        if pending.session_id != completed.session_id || pending.run_id != completed.run_id {
            return Err(conflict("pending tool exchange ownership does not match"));
        }
        if !matches!(pending.state, PendingState::Begun) {
            return Err(conflict("pending tool exchange is already ready"));
        }
        validate_exchange(&pending.assistant, &completed.results)?;
        self.mark_tool_exchange_ready(&pending, &completed.results)?;
        self.commit_ready_tool_exchange(
            completed.operation_id,
            pending,
            completed.results,
            completed.completed_at_ms,
        )
    }

    /// body_appends 先恢复；仍存在的 pending 再按 begun/ready 语义补成完整正文。
    pub(super) fn recover_pending_tool_exchanges(&mut self) -> StorageResult<HashSet<String>> {
        let receipts = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT receipt_id, session_id
                     FROM pending_tool_exchanges ORDER BY created_at_ms, receipt_id",
                )
                .map_err(|source| {
                    internal_error("pending tool exchanges could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| {
                    internal_error("pending tool exchanges could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("pending tool exchange row could not be read", source)
            })?
        };

        let mut unavailable = HashSet::new();
        for (receipt_id, session_id) in receipts {
            if self.recover_pending_tool_exchange(&receipt_id).is_err() {
                unavailable.insert(session_id);
            }
        }
        Ok(unavailable)
    }

    fn recover_pending_tool_exchange(&mut self, receipt_id: &str) -> StorageResult<()> {
        let pending = self.load_pending_exchange(receipt_id)?;
        let results = match pending.state {
            PendingState::Begun => {
                let results = unknown_results(&pending)?;
                self.mark_tool_exchange_ready(&pending, &results)?;
                results
            }
            PendingState::Ready => pending
                .results
                .clone()
                .ok_or_else(|| invalid_data("ready tool exchange has no results"))?,
        };
        validate_exchange(&pending.assistant, &results)?;
        self.commit_ready_tool_exchange(
            format!("recover-{}", pending.receipt.as_str()),
            pending,
            results,
            system_time_ms()?,
        )
    }

    fn mark_tool_exchange_ready(
        &mut self,
        pending: &StoredPendingExchange,
        results: &[ToolMessage],
    ) -> StorageResult<()> {
        let results_json = serde_json::to_string(results)
            .map_err(|source| internal_error("tool results could not be encoded", source))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("tool exchange ready transition could not begin", source)
            })?;
        let updated = transaction
            .execute(
                "UPDATE pending_tool_exchanges
                 SET results_json = ?1, state = 'ready'
                 WHERE receipt_id = ?2 AND session_id = ?3 AND run_id = ?4 AND state = 'begun'",
                params![
                    results_json,
                    pending.receipt.as_str(),
                    pending.session_id.as_str(),
                    pending.run_id.as_str(),
                ],
            )
            .map_err(|source| internal_error("tool exchange could not become ready", source))?;
        if updated != 1 {
            return Err(conflict("tool exchange is not in begun state"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("tool exchange ready state could not be committed", source)
        })
    }

    fn commit_ready_tool_exchange(
        &mut self,
        operation_id: String,
        pending: StoredPendingExchange,
        results: Vec<ToolMessage>,
        completed_at_ms: i64,
    ) -> StorageResult<()> {
        let mut messages = vec![ConversationMessage::Assistant(pending.assistant)];
        messages.extend(results.into_iter().map(ConversationMessage::Tool));
        let operation = operation_id.clone();
        self.stage_append_for(
            AppendRequest {
                operation_id,
                session_id: pending.session_id,
                run_id: pending.run_id,
                messages,
                created_at_ms: completed_at_ms,
            },
            AppendPurpose::ToolExchange {
                receipt_id: pending.receipt.as_str().to_owned(),
            },
        )?;
        self.complete_staged_append(&operation)
    }

    fn load_pending_exchange(&self, receipt_id: &str) -> StorageResult<StoredPendingExchange> {
        let row = self
            .connection
            .query_row(
                "SELECT receipt_id, session_id, run_id, assistant_json, results_json, state
                 FROM pending_tool_exchanges WHERE receipt_id = ?1",
                [receipt_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("pending tool exchange could not be queried", source))?
            .ok_or_else(|| conflict("pending tool exchange does not exist"))?;
        let state = match row.5.as_str() {
            "begun" => PendingState::Begun,
            "ready" => PendingState::Ready,
            _ => return Err(invalid_data("pending tool exchange state is invalid")),
        };
        let assistant = serde_json::from_str::<AssistantMessage>(&row.3).map_err(|source| {
            invalid_data_with_source("pending assistant message is invalid", source)
        })?;
        let results = row
            .4
            .map(|json| {
                serde_json::from_str::<Vec<ToolMessage>>(&json).map_err(|source| {
                    invalid_data_with_source("pending tool results are invalid", source)
                })
            })
            .transpose()?;
        Ok(StoredPendingExchange {
            receipt: ExchangeReceipt::new(row.0).map_err(|source| {
                invalid_data_with_source("pending tool exchange receipt is invalid", source)
            })?,
            session_id: SessionId::new(row.1).map_err(|source| {
                invalid_data_with_source("pending tool exchange session id is invalid", source)
            })?,
            run_id: RunId::new(row.2).map_err(|source| {
                invalid_data_with_source("pending tool exchange run id is invalid", source)
            })?,
            assistant,
            results,
            state,
        })
    }
}

fn validate_assistant_calls(assistant: &AssistantMessage) -> StorageResult<()> {
    let mut seen = HashSet::new();
    let call_ids = assistant
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if call_ids.is_empty() {
        return Err(assistant_runtime::StoreError::new(
            assistant_runtime::StoreErrorKind::InvalidInput,
            "pending assistant message has no tool calls",
        ));
    }
    if !call_ids
        .iter()
        .all(|call_id| seen.insert((*call_id).to_owned()))
    {
        return Err(assistant_runtime::StoreError::new(
            assistant_runtime::StoreErrorKind::InvalidInput,
            "pending assistant message has duplicate tool calls",
        ));
    }
    Ok(())
}

fn validate_exchange(assistant: &AssistantMessage, results: &[ToolMessage]) -> StorageResult<()> {
    validate_assistant_calls(assistant)?;
    ConversationSnapshot::new(
        std::iter::once(ConversationMessage::Assistant(assistant.clone()))
            .chain(results.iter().cloned().map(ConversationMessage::Tool))
            .collect(),
    )
    .validate_tool_exchange_pairs()
    .map_err(|source| {
        assistant_runtime::StoreError::with_source(
            assistant_runtime::StoreErrorKind::InvalidInput,
            "tool exchange batch is invalid",
            source,
        )
    })
}

fn unknown_results(pending: &StoredPendingExchange) -> StorageResult<Vec<ToolMessage>> {
    pending
        .assistant
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call),
            _ => None,
        })
        .enumerate()
        .map(|(index, call)| {
            Ok(ToolMessage {
                id: MessageId::new(format!(
                    "recovered-{}-{}",
                    pending.receipt.as_str(),
                    index + 1
                ))
                .map_err(|source| {
                    invalid_data_with_source("recovered tool message id is invalid", source)
                })?,
                result: ToolResult {
                    call_id: call.id.clone(),
                    status: ToolResultStatus::Error,
                    content: ToolResultContent::Text(UNKNOWN_OUTCOME_TEXT.to_owned()),
                },
            })
        })
        .collect()
}
