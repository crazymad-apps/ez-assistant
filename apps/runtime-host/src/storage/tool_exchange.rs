//! 主 Run 与子任务共用的两阶段工具交换持久化。
//!
//! `PendingTarget` 只适配所有权、表名与最终 Conversation 目标；begin/started/ready/commit 和
//! 重启补偿算法只有这一份，避免两个执行层产生不同的副作用边界。

use std::collections::HashSet;

use agent_core::ExchangeReceipt;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, MessageId,
    ToolMessage, ToolResult, ToolResultContent, ToolResultStatus, TranscriptVisibility,
    UserMessage, UserMessageOrigin, UserPart,
};
use assistant_protocol::{ChildTaskId, ChildTaskStatus, RunId, SessionId};
use assistant_runtime::{
    ChildToolExecutionStart, CompletedChildToolExchange, CompletedToolExchange,
    PendingChildToolExchange, PendingToolExchange, SkillActivationOwner, SkillActivationTrigger,
    StoredSkillActivation, ToolExecutionStart,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::{
    StorageEngine, StorageResult, append_effect::AppendPurpose, conflict, database_write_error,
    internal_error, invalid_data, invalid_data_with_source, recovery::AppendRequest,
    run_state::system_time_ms,
};

const UNKNOWN_OUTCOME_TEXT: &str = "runtime restarted; tool execution outcome is unknown";
const NOT_STARTED_TEXT: &str = "runtime restarted before tool execution started";

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingTarget {
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
}

struct StoredPendingExchange {
    receipt: ExchangeReceipt,
    target: PendingTarget,
    step: Option<u32>,
    assistant: AssistantMessage,
    ready: Option<ReadyExchangePayload>,
    started_calls: HashSet<String>,
    state: PendingState,
}

/// ready 状态必须同时冻结 Tool Results 与可选 Activation，保证崩溃恢复不拆分两者。
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReadyExchangePayload {
    results: Vec<ToolMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation_message: Option<UserMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skill_activations: Vec<StoredSkillActivation>,
}

#[derive(Clone, Copy)]
enum PendingState {
    Begun,
    Ready,
}

#[derive(Clone, Copy)]
enum PendingTable {
    Run,
    ChildTask,
}

struct CompleteExchangeRequest {
    operation_id: String,
    receipt: ExchangeReceipt,
    target: PendingTarget,
    expected_step: Option<u32>,
    results: Vec<ToolMessage>,
    activation_message: Option<UserMessage>,
    skill_activations: Vec<StoredSkillActivation>,
    completed_at_ms: i64,
    table: PendingTable,
}

impl StorageEngine {
    pub(super) fn begin_tool_exchange(
        &mut self,
        pending: PendingToolExchange,
    ) -> StorageResult<()> {
        self.begin_exchange(
            pending.receipt,
            PendingTarget::Run {
                session_id: pending.session_id,
                run_id: pending.run_id,
            },
            Some(pending.step),
            pending.assistant,
            pending.created_at_ms,
        )
    }

    pub(super) fn begin_child_tool_exchange(
        &mut self,
        pending: PendingChildToolExchange,
    ) -> StorageResult<()> {
        self.begin_exchange(
            pending.receipt,
            PendingTarget::ChildTask {
                session_id: pending.session_id,
                child_task_id: pending.child_task_id,
            },
            Some(pending.step),
            pending.assistant,
            pending.created_at_ms,
        )
    }

    pub(super) fn mark_tool_execution_started(
        &mut self,
        start: ToolExecutionStart,
    ) -> StorageResult<()> {
        self.mark_execution_started(
            start.receipt,
            PendingTarget::Run {
                session_id: start.session_id,
                run_id: start.run_id,
            },
            start.call_id.as_str(),
            start.started_at_ms,
            PendingTable::Run,
        )
    }

    pub(super) fn mark_child_tool_execution_started(
        &mut self,
        start: ChildToolExecutionStart,
    ) -> StorageResult<()> {
        self.mark_execution_started(
            start.receipt,
            PendingTarget::ChildTask {
                session_id: start.session_id,
                child_task_id: start.child_task_id,
            },
            start.call_id.as_str(),
            start.started_at_ms,
            PendingTable::ChildTask,
        )
    }

    pub(super) fn complete_tool_exchange(
        &mut self,
        completed: CompletedToolExchange,
    ) -> StorageResult<()> {
        self.complete_exchange(CompleteExchangeRequest {
            operation_id: completed.operation_id,
            receipt: completed.receipt,
            target: PendingTarget::Run {
                session_id: completed.session_id,
                run_id: completed.run_id,
            },
            expected_step: Some(completed.step),
            results: completed.results,
            activation_message: completed.activation_message,
            skill_activations: completed.skill_activations,
            completed_at_ms: completed.completed_at_ms,
            table: PendingTable::Run,
        })
    }

    pub(super) fn complete_child_tool_exchange(
        &mut self,
        completed: CompletedChildToolExchange,
    ) -> StorageResult<()> {
        self.complete_exchange(CompleteExchangeRequest {
            operation_id: completed.operation_id,
            receipt: completed.receipt,
            target: PendingTarget::ChildTask {
                session_id: completed.session_id,
                child_task_id: completed.child_task_id,
            },
            expected_step: Some(completed.step),
            results: completed.results,
            activation_message: completed.activation_message,
            skill_activations: completed.skill_activations,
            completed_at_ms: completed.completed_at_ms,
            table: PendingTable::ChildTask,
        })
    }

    pub(super) fn recover_pending_tool_exchanges(&mut self) -> StorageResult<HashSet<String>> {
        self.recover_pending_exchanges(PendingTable::Run)
    }

    pub(super) fn recover_pending_child_tool_exchanges(
        &mut self,
    ) -> StorageResult<HashSet<String>> {
        self.recover_pending_exchanges(PendingTable::ChildTask)
    }

    fn begin_exchange(
        &mut self,
        receipt: ExchangeReceipt,
        target: PendingTarget,
        step: Option<u32>,
        assistant: AssistantMessage,
        created_at_ms: i64,
    ) -> StorageResult<()> {
        validate_assistant_calls(&assistant)?;
        self.ensure_pending_target_running(&target)?;
        let assistant_json = serde_json::to_string(&assistant).map_err(|source| {
            internal_error("pending assistant message could not be encoded", source)
        })?;
        match &target {
            PendingTarget::Run { session_id, run_id } => self.connection.execute(
                "INSERT INTO pending_tool_exchanges (
                    receipt_id, session_id, run_id, step, assistant_json, results_json, state, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'begun', ?6)",
                params![receipt.as_str(), session_id.as_str(), run_id.as_str(), step, assistant_json, created_at_ms],
            ),
            PendingTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.connection.execute(
                "INSERT INTO child_pending_tool_exchanges (
                    receipt_id, child_task_id, session_id, step, assistant_json, results_json, state,
                    created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'begun', ?6)",
                params![receipt.as_str(), child_task_id.as_str(), session_id.as_str(), step, assistant_json, created_at_ms],
            ),
        }
        .map_err(|source| database_write_error("pending tool exchange could not be created", source))?;
        Ok(())
    }

    fn mark_execution_started(
        &mut self,
        receipt: ExchangeReceipt,
        target: PendingTarget,
        call_id: &str,
        started_at_ms: i64,
        table: PendingTable,
    ) -> StorageResult<()> {
        let pending = self.load_pending_exchange(receipt.as_str(), table)?;
        if pending.target != target {
            return Err(conflict("tool execution start ownership does not match"));
        }
        if !matches!(pending.state, PendingState::Begun) {
            return Err(conflict(
                "tool execution cannot start after results are ready",
            ));
        }
        if !pending.assistant.parts.iter().any(
            |part| matches!(part, AssistantPart::ToolCall(call) if call.id.as_str() == call_id),
        ) {
            return Err(conflict(
                "tool execution start does not belong to pending exchange",
            ));
        }
        if pending.started_calls.contains(call_id) {
            return Err(conflict("tool execution start is already recorded"));
        }
        let query = match table {
            PendingTable::Run => {
                "INSERT INTO pending_tool_starts (receipt_id, call_id, started_at_ms)
                 VALUES (?1, ?2, ?3)"
            }
            PendingTable::ChildTask => {
                "INSERT INTO child_pending_tool_starts (receipt_id, call_id, started_at_ms)
                 VALUES (?1, ?2, ?3)"
            }
        };
        self.connection
            .execute(query, params![receipt.as_str(), call_id, started_at_ms])
            .map_err(|source| {
                database_write_error("tool execution start could not be recorded", source)
            })?;
        Ok(())
    }

    fn complete_exchange(&mut self, request: CompleteExchangeRequest) -> StorageResult<()> {
        let pending = self.load_pending_exchange(request.receipt.as_str(), request.table)?;
        if pending.target != request.target || pending.step != request.expected_step {
            return Err(conflict("pending tool exchange ownership does not match"));
        }
        if !matches!(pending.state, PendingState::Begun) {
            return Err(conflict("pending tool exchange is already ready"));
        }
        validate_exchange(&pending.assistant, &request.results)?;
        validate_model_activations(
            &pending.target,
            request.activation_message.as_ref(),
            &request.skill_activations,
        )?;
        self.ensure_activation_runs_match(&pending.target, &request.skill_activations)?;
        let ready = ReadyExchangePayload {
            results: request.results,
            activation_message: request.activation_message,
            skill_activations: request.skill_activations,
        };
        self.mark_exchange_ready(&pending, &ready, request.table)?;
        self.commit_ready_exchange(
            request.operation_id,
            pending,
            ready,
            request.completed_at_ms,
        )
    }

    fn recover_pending_exchanges(&mut self, table: PendingTable) -> StorageResult<HashSet<String>> {
        let query = match table {
            PendingTable::Run => {
                "SELECT receipt_id, session_id FROM pending_tool_exchanges
                 ORDER BY created_at_ms, receipt_id"
            }
            PendingTable::ChildTask => {
                "SELECT receipt_id, child_task_id FROM child_pending_tool_exchanges
                 ORDER BY created_at_ms, receipt_id"
            }
        };
        let pending = {
            let mut statement = self.connection.prepare(query).map_err(|source| {
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
        for (receipt_id, owner_id) in pending {
            if self.recover_pending_exchange(&receipt_id, table).is_err() {
                unavailable.insert(owner_id);
            }
        }
        Ok(unavailable)
    }

    fn recover_pending_exchange(
        &mut self,
        receipt_id: &str,
        table: PendingTable,
    ) -> StorageResult<()> {
        let pending = self.load_pending_exchange(receipt_id, table)?;
        let ready = match pending.state {
            PendingState::Begun => {
                let results = self.recovered_results(&pending)?;
                let ready = ReadyExchangePayload {
                    results,
                    activation_message: None,
                    skill_activations: Vec::new(),
                };
                self.mark_exchange_ready(&pending, &ready, table)?;
                ready
            }
            PendingState::Ready => pending
                .ready
                .clone()
                .ok_or_else(|| invalid_data("ready tool exchange has no payload"))?,
        };
        validate_exchange(&pending.assistant, &ready.results)?;
        validate_model_activations(
            &pending.target,
            ready.activation_message.as_ref(),
            &ready.skill_activations,
        )?;
        self.ensure_activation_runs_match(&pending.target, &ready.skill_activations)?;
        self.commit_ready_exchange(
            format!("recover-{}", pending.receipt.as_str()),
            pending,
            ready,
            system_time_ms()?,
        )
    }

    fn recovered_results(
        &self,
        pending: &StoredPendingExchange,
    ) -> StorageResult<Vec<ToolMessage>> {
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
                if call.name.as_str() == assistant_runtime::DELEGATE_TASK_TOOL_NAME
                    && pending.started_calls.contains(call.id.as_str())
                    && let PendingTarget::Run { session_id, run_id } = &pending.target
                {
                    return self.recovered_delegate_result(
                        pending.receipt.as_str(),
                        index,
                        session_id,
                        run_id,
                        call,
                    );
                }
                recovered_unknown_result(
                    pending.receipt.as_str(),
                    index,
                    call,
                    pending.started_calls.contains(call.id.as_str()),
                )
            })
            .collect()
    }

    fn recovered_delegate_result(
        &self,
        receipt_id: &str,
        index: usize,
        session_id: &SessionId,
        run_id: &RunId,
        call: &agent_types::ToolCall,
    ) -> StorageResult<ToolMessage> {
        let row = self
            .connection
            .query_row(
                "SELECT child_task_id, status, final_message_id, error_code
                 FROM child_tasks
                 WHERE session_id = ?1 AND parent_run_id = ?2 AND parent_tool_call_id = ?3",
                params![session_id.as_str(), run_id.as_str(), call.id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("delegated child result could not be queried", source)
            })?;
        let id = recovered_message_id(receipt_id, index)?;
        let Some((child_task_id, status, final_message_id, error_code)) = row else {
            return Ok(delegation_error_result(
                id,
                call,
                None,
                "interrupted",
                "interrupted",
            ));
        };
        let child_task_id = ChildTaskId::new(child_task_id).map_err(|source| {
            invalid_data_with_source("delegated child task id is invalid", source)
        })?;
        let status = super::mode::parse_child_task_status(&status)?;
        if status == ChildTaskStatus::Completed {
            let final_message_id = final_message_id
                .ok_or_else(|| invalid_data("completed delegated child has no final message"))?;
            let conversation = self.load_child_conversation(session_id, &child_task_id)?;
            let result = conversation
                .messages
                .iter()
                .find_map(|message| match message {
                    ConversationMessage::Assistant(message)
                        if message.id.as_str() == final_message_id =>
                    {
                        Some(
                            message
                                .parts
                                .iter()
                                .filter_map(|part| match part {
                                    AssistantPart::Text(text) => Some(text.text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        )
                    }
                    _ => None,
                })
                .ok_or_else(|| invalid_data("delegated child final message is unavailable"))?;
            return Ok(ToolMessage {
                id,
                result: ToolResult {
                    call_id: call.id.clone(),
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::json(serde_json::json!({
                        "task_id": child_task_id.as_str(),
                        "status": "completed",
                        "result": result,
                    })),
                    metadata: None,
                },
            });
        }
        let status_code = child_status_code(status);
        let code = error_code.unwrap_or_else(|| status_code.to_owned());
        Ok(delegation_error_result(
            id,
            call,
            Some(child_task_id.as_str()),
            status_code,
            &code,
        ))
    }

    fn mark_exchange_ready(
        &mut self,
        pending: &StoredPendingExchange,
        ready: &ReadyExchangePayload,
        table: PendingTable,
    ) -> StorageResult<()> {
        let results_json = serde_json::to_string(ready).map_err(|source| {
            internal_error("tool exchange payload could not be encoded", source)
        })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("tool exchange ready transition could not begin", source)
            })?;
        let changed = match &pending.target {
            PendingTarget::Run { session_id, run_id } if matches!(table, PendingTable::Run) => {
                transaction.execute(
                    "UPDATE pending_tool_exchanges SET results_json = ?1, state = 'ready'
                     WHERE receipt_id = ?2 AND session_id = ?3 AND run_id = ?4 AND state = 'begun'",
                    params![results_json, pending.receipt.as_str(), session_id.as_str(), run_id.as_str()],
                )
            }
            PendingTarget::ChildTask {
                session_id,
                child_task_id,
            } if matches!(table, PendingTable::ChildTask) => transaction.execute(
                "UPDATE child_pending_tool_exchanges SET results_json = ?1, state = 'ready'
                 WHERE receipt_id = ?2 AND child_task_id = ?3 AND session_id = ?4 AND state = 'begun'",
                params![results_json, pending.receipt.as_str(), child_task_id.as_str(), session_id.as_str()],
            ),
            _ => return Err(invalid_data("pending tool exchange table does not match target")),
        }
        .map_err(|source| internal_error("tool exchange could not become ready", source))?;
        if changed != 1 {
            return Err(conflict("tool exchange is not in begun state"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("tool exchange ready state could not be committed", source)
        })
    }

    fn commit_ready_exchange(
        &mut self,
        operation_id: String,
        pending: StoredPendingExchange,
        ready: ReadyExchangePayload,
        completed_at_ms: i64,
    ) -> StorageResult<()> {
        let mut messages = vec![ConversationMessage::Assistant(pending.assistant)];
        messages.extend(ready.results.into_iter().map(ConversationMessage::Tool));
        if let Some(message) = ready.activation_message {
            messages.push(ConversationMessage::User(message));
        }
        match pending.target {
            PendingTarget::Run { session_id, run_id } => {
                let operation = operation_id.clone();
                self.stage_append_for(
                    AppendRequest {
                        operation_id,
                        session_id,
                        run_id,
                        messages,
                        message_step: pending.step,
                        created_at_ms: completed_at_ms,
                    },
                    AppendPurpose::ToolExchange {
                        receipt_id: pending.receipt.as_str().to_owned(),
                        skill_activations: ready.skill_activations,
                    },
                )?;
                self.complete_staged_append(&operation)
            }
            PendingTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.append_child_messages(super::recovery::ChildAppendRequest {
                operation_id,
                child_task_id,
                session_id,
                messages,
                message_step: pending.step,
                created_at_ms: completed_at_ms,
                purpose: AppendPurpose::ChildToolExchange {
                    receipt_id: pending.receipt.as_str().to_owned(),
                    skill_activations: ready.skill_activations,
                },
            }),
        }
    }

    fn load_pending_exchange(
        &self,
        receipt_id: &str,
        table: PendingTable,
    ) -> StorageResult<StoredPendingExchange> {
        let query = match table {
            PendingTable::Run => {
                "SELECT receipt_id, session_id, run_id, step, assistant_json, results_json, state
                 FROM pending_tool_exchanges WHERE receipt_id = ?1"
            }
            PendingTable::ChildTask => {
                "SELECT receipt_id, session_id, child_task_id, step, assistant_json, results_json, state
                 FROM child_pending_tool_exchanges WHERE receipt_id = ?1"
            }
        };
        let row = self
            .connection
            .query_row(query, [receipt_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .optional()
            .map_err(|source| internal_error("pending tool exchange could not be queried", source))?
            .ok_or_else(|| conflict("pending tool exchange does not exist"))?;
        let target = match table {
            PendingTable::Run => PendingTarget::Run {
                session_id: SessionId::new(row.1).map_err(|source| {
                    invalid_data_with_source("pending tool exchange session id is invalid", source)
                })?,
                run_id: RunId::new(row.2).map_err(|source| {
                    invalid_data_with_source("pending tool exchange run id is invalid", source)
                })?,
            },
            PendingTable::ChildTask => PendingTarget::ChildTask {
                session_id: SessionId::new(row.1).map_err(|source| {
                    invalid_data_with_source("pending tool exchange session id is invalid", source)
                })?,
                child_task_id: ChildTaskId::new(row.2).map_err(|source| {
                    invalid_data_with_source("pending child task id is invalid", source)
                })?,
            },
        };
        let step = row
            .3
            .map(|value| {
                u32::try_from(value)
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| invalid_data("pending tool exchange step is invalid"))
            })
            .transpose()?;
        let assistant = serde_json::from_str::<AssistantMessage>(&row.4).map_err(|source| {
            invalid_data_with_source("pending assistant message is invalid", source)
        })?;
        let ready = row
            .5
            .map(|json| {
                serde_json::from_str::<ReadyExchangePayload>(&json)
                    .or_else(|_| {
                        serde_json::from_str::<Vec<ToolMessage>>(&json).map(|results| {
                            ReadyExchangePayload {
                                results,
                                activation_message: None,
                                skill_activations: Vec::new(),
                            }
                        })
                    })
                    .map_err(|source| {
                        invalid_data_with_source("pending tool exchange payload is invalid", source)
                    })
            })
            .transpose()?;
        let state = match row.6.as_str() {
            "begun" => PendingState::Begun,
            "ready" => PendingState::Ready,
            _ => return Err(invalid_data("pending tool exchange state is invalid")),
        };
        let started_query = match table {
            PendingTable::Run => {
                "SELECT call_id FROM pending_tool_starts WHERE receipt_id = ?1 ORDER BY call_id"
            }
            PendingTable::ChildTask => {
                "SELECT call_id FROM child_pending_tool_starts
                 WHERE receipt_id = ?1 ORDER BY call_id"
            }
        };
        let started_calls = {
            let mut statement = self.connection.prepare(started_query).map_err(|source| {
                internal_error("pending tool starts could not be queried", source)
            })?;
            let rows = statement
                .query_map([receipt_id], |row| row.get::<_, String>(0))
                .map_err(|source| {
                    internal_error("pending tool starts could not be read", source)
                })?;
            rows.collect::<Result<HashSet<_>, _>>().map_err(|source| {
                internal_error("pending tool start row could not be read", source)
            })?
        };
        if started_calls.iter().any(|call_id| {
            !assistant.parts.iter().any(
                |part| matches!(part, AssistantPart::ToolCall(call) if call.id.as_str() == call_id),
            )
        }) {
            return Err(invalid_data(
                "pending tool start does not belong to assistant message",
            ));
        }
        Ok(StoredPendingExchange {
            receipt: ExchangeReceipt::new(row.0).map_err(|source| {
                invalid_data_with_source("pending tool exchange receipt is invalid", source)
            })?,
            target,
            step,
            assistant,
            ready,
            started_calls,
            state,
        })
    }

    fn ensure_pending_target_running(&self, target: &PendingTarget) -> StorageResult<()> {
        match target {
            PendingTarget::Run { session_id, run_id } => {
                let status = self
                    .connection
                    .query_row(
                        "SELECT status FROM runs WHERE run_id = ?1 AND session_id = ?2",
                        params![run_id.as_str(), session_id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|source| {
                        internal_error("tool exchange run could not be queried", source)
                    })?
                    .ok_or_else(|| conflict("tool exchange run does not exist"))?;
                if status != "running" {
                    return Err(conflict("run cannot begin a tool exchange"));
                }
                Ok(())
            }
            PendingTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.ensure_child_status(
                session_id,
                child_task_id,
                assistant_protocol::ChildTaskStatus::Running,
            ),
        }
    }

    fn ensure_activation_runs_match(
        &self,
        target: &PendingTarget,
        activations: &[StoredSkillActivation],
    ) -> StorageResult<()> {
        if activations.is_empty() {
            return Ok(());
        }
        let expected_run_id = match target {
            PendingTarget::Run { run_id, .. } => run_id.clone(),
            PendingTarget::ChildTask {
                session_id,
                child_task_id,
            } => self
                .connection
                .query_row(
                    "SELECT parent_run_id FROM child_tasks \
                     WHERE child_task_id = ?1 AND session_id = ?2",
                    params![child_task_id.as_str(), session_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|source| {
                    internal_error("child activation run could not be queried", source)
                })?
                .ok_or_else(|| invalid_data("child activation owner is unavailable"))
                .and_then(|value| {
                    RunId::new(value).map_err(|source| {
                        invalid_data_with_source("child activation run id is invalid", source)
                    })
                })?,
        };
        if activations
            .iter()
            .any(|activation| activation.run_id.as_ref() != Some(&expected_run_id))
        {
            return Err(invalid_data("model skill activation run is inconsistent"));
        }
        Ok(())
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

fn validate_model_activations(
    target: &PendingTarget,
    message: Option<&UserMessage>,
    activations: &[StoredSkillActivation],
) -> StorageResult<()> {
    if activations.is_empty() {
        return if message.is_none() {
            Ok(())
        } else {
            Err(invalid_data(
                "tool exchange has an unbound skill activation message",
            ))
        };
    }
    let message =
        message.ok_or_else(|| invalid_data("model skill activation message is missing"))?;
    let activation_ids = activations
        .iter()
        .map(|activation| activation.activation_id.as_str())
        .collect::<HashSet<_>>();
    let names = activations
        .iter()
        .map(|activation| activation.name.as_str())
        .collect::<HashSet<_>>();
    if message.origin != UserMessageOrigin::Runtime
        || message.transcript_visibility != TranscriptVisibility::Hidden
        || activation_ids.len() != activations.len()
        || names.len() != activations.len()
    {
        return Err(invalid_data("model skill activation message is invalid"));
    }
    let (session_id, expected_owner) = match target {
        PendingTarget::Run { session_id, .. } => (
            session_id,
            SkillActivationOwner::Session(session_id.clone()),
        ),
        PendingTarget::ChildTask {
            session_id,
            child_task_id,
        } => (
            session_id,
            SkillActivationOwner::ChildTask(child_task_id.as_str().to_owned()),
        ),
    };
    if activations.iter().any(|activation| {
        activation.session_id != *session_id
            || activation.owner != expected_owner
            || activation.trigger != SkillActivationTrigger::Model
            || activation.run_id.is_none()
            || activation.input_id.is_some()
            || activation.message_id != message.id
    }) {
        return Err(invalid_data("model skill activation is inconsistent"));
    }
    for activation in activations {
        let count = message
            .parts
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    UserPart::InternalContext(part)
                        if part.kind == "skill_activation"
                            && part.retention_key.as_deref()
                                == Some(&format!("skill:{}", activation.name.as_str()))
                )
            })
            .count();
        if count != 1 {
            return Err(invalid_data(
                "model skill activation boundary is inconsistent",
            ));
        }
    }
    Ok(())
}

fn recovered_unknown_result(
    receipt_id: &str,
    index: usize,
    call: &agent_types::ToolCall,
    started: bool,
) -> StorageResult<ToolMessage> {
    Ok(ToolMessage {
        id: recovered_message_id(receipt_id, index)?,
        result: ToolResult {
            call_id: call.id.clone(),
            status: ToolResultStatus::Error,
            content: ToolResultContent::text(
                if started {
                    UNKNOWN_OUTCOME_TEXT
                } else {
                    NOT_STARTED_TEXT
                }
                .to_owned(),
            ),
            metadata: None,
        },
    })
}

fn recovered_message_id(receipt_id: &str, index: usize) -> StorageResult<MessageId> {
    MessageId::new(format!("recovered-{receipt_id}-{}", index + 1))
        .map_err(|source| invalid_data_with_source("recovered tool message id is invalid", source))
}

fn delegation_error_result(
    id: MessageId,
    call: &agent_types::ToolCall,
    child_task_id: Option<&str>,
    status: &str,
    code: &str,
) -> ToolMessage {
    ToolMessage {
        id,
        result: ToolResult {
            call_id: call.id.clone(),
            status: ToolResultStatus::Error,
            content: ToolResultContent::json(serde_json::json!({
                "error": {
                    "message": "child task did not complete",
                    "details": {
                        "task_id": child_task_id,
                        "status": status,
                        "code": code,
                    }
                }
            })),
            metadata: None,
        },
    }
}

fn child_status_code(status: ChildTaskStatus) -> &'static str {
    match status {
        ChildTaskStatus::Accepted | ChildTaskStatus::Running | ChildTaskStatus::Interrupted => {
            "interrupted"
        }
        ChildTaskStatus::Completed => "completed",
        ChildTaskStatus::Failed => "failed",
        ChildTaskStatus::Cancelled => "cancelled",
    }
}
