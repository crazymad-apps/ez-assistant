//! staged append 的持久业务意图及其 SQLite 提交效果。

use assistant_protocol::{
    ChildTaskId, ChildTaskStatus, RunId, RunStatus, RuntimeErrorInfo, SessionId,
};
use assistant_runtime::{NewStoredInput, StoredGoalSettlementEffect, StoredSkillActivation};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use super::{
    StorageResult, conflict,
    goal::apply_goal_settlement,
    input_state::insert_goal_continuation,
    internal_error, invalid_data, invalid_data_with_source,
    run_projection::{error_code_value, run_status_value},
};

/// Conversation 文件写入成功后，必须在同一 SQLite 事务应用的业务转换。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AppendPurpose {
    /// 只追加规范消息，不改变 Input/Run 状态。
    Messages,
    /// 首次 User Message 已写入，提交 Input 并启动 Run。
    UserMessage {
        #[serde(default)]
        reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    },
    /// 完整工具批次已写入；清除对应的临时 pending 事实。
    ToolExchange {
        receipt_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skill_activations: Vec<StoredSkillActivation>,
    },
    /// 最终消息已写入，结算 Run 终态。
    RunSettlement {
        status: RunStatus,
        cancel_requested: bool,
        error: Option<RuntimeErrorInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_effect: Option<Box<StoredGoalSettlementEffect>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy_report: Option<Box<NewStoredInput>>,
    },
    /// 子任务初始 User Message 已写入，切换到 running。
    ChildStart,
    /// 子任务完整工具批次已写入，清除 child pending 事实。
    ChildToolExchange {
        receipt_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skill_activations: Vec<StoredSkillActivation>,
    },
    /// 子任务最终消息已写入，结算 child 终态。
    ChildSettlement {
        status: ChildTaskStatus,
        cancel_requested: bool,
        error: Option<RuntimeErrorInfo>,
        final_message_id: Option<agent_types::MessageId>,
    },
}

/// 同一 staged append 算法的两种业务目标；物理表和路径只在 Host 内部适配。
#[derive(Clone, Debug)]
pub(super) enum ConversationStorageTarget {
    Session {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
}

pub(super) fn encode_purpose(purpose: &AppendPurpose) -> StorageResult<String> {
    if matches!(purpose, AppendPurpose::Messages) {
        return Ok("messages".to_owned());
    }
    serde_json::to_string(purpose)
        .map_err(|source| internal_error("staged append purpose could not be encoded", source))
}

pub(super) fn decode_purpose(value: &str) -> StorageResult<AppendPurpose> {
    if value == "messages" {
        return Ok(AppendPurpose::Messages);
    }
    serde_json::from_str(value)
        .map_err(|source| invalid_data_with_source("staged append purpose is invalid", source))
}

pub(super) fn apply_purpose(
    transaction: &Transaction<'_>,
    purpose: &AppendPurpose,
    target: &ConversationStorageTarget,
    created_at_ms: i64,
) -> StorageResult<()> {
    match (purpose, target) {
        (AppendPurpose::Messages, _) => Ok(()),
        (
            AppendPurpose::UserMessage { reasoning_effort },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_user_message_start(
            transaction,
            run_id,
            session_id,
            *reasoning_effort,
            created_at_ms,
        ),
        (
            AppendPurpose::ToolExchange {
                receipt_id,
                skill_activations,
            },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => {
            let deleted = transaction
                .execute(
                    "DELETE FROM pending_tool_exchanges
                     WHERE receipt_id = ?1 AND run_id = ?2 AND session_id = ?3 AND state = 'ready'",
                    params![receipt_id, run_id.as_str(), session_id.as_str()],
                )
                .map_err(|source| {
                    internal_error("pending tool exchange could not be cleared", source)
                })?;
            if deleted != 1 {
                return Err(invalid_data(
                    "pending tool exchange finalization is inconsistent",
                ));
            }
            for activation in skill_activations {
                super::skill::insert_skill_activation(transaction, activation)?;
            }
            Ok(())
        }
        (
            AppendPurpose::RunSettlement { .. },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_run_settlement(transaction, run_id, session_id, purpose, created_at_ms),
        (
            AppendPurpose::ChildStart,
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            },
        ) => apply_child_start(transaction, child_task_id, session_id, created_at_ms),
        (
            AppendPurpose::ChildToolExchange {
                receipt_id,
                skill_activations,
            },
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            },
        ) => {
            clear_child_exchange(transaction, receipt_id, child_task_id, session_id)?;
            for activation in skill_activations {
                super::skill::insert_skill_activation(transaction, activation)?;
            }
            Ok(())
        }
        (
            AppendPurpose::ChildSettlement { .. },
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            },
        ) => apply_child_settlement(
            transaction,
            child_task_id,
            session_id,
            purpose,
            created_at_ms,
        ),
        _ => Err(invalid_data(
            "append purpose does not match conversation target",
        )),
    }
}

fn apply_child_start(
    transaction: &Transaction<'_>,
    child_task_id: &ChildTaskId,
    session_id: &SessionId,
    started_at_ms: i64,
) -> StorageResult<()> {
    let changed = transaction
        .execute(
            "UPDATE child_tasks SET status = 'running', started_at_ms = ?1
             WHERE child_task_id = ?2 AND session_id = ?3 AND status = 'accepted'",
            params![started_at_ms, child_task_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| internal_error("child task could not be started", source))?;
    if changed != 1 {
        return Err(conflict("child task cannot be started"));
    }
    Ok(())
}

fn clear_child_exchange(
    transaction: &Transaction<'_>,
    receipt_id: &str,
    child_task_id: &ChildTaskId,
    session_id: &SessionId,
) -> StorageResult<()> {
    let deleted = transaction
        .execute(
            "DELETE FROM child_pending_tool_exchanges
             WHERE receipt_id = ?1 AND child_task_id = ?2 AND session_id = ?3 AND state = 'ready'",
            params![receipt_id, child_task_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| {
            internal_error("pending child tool exchange could not be cleared", source)
        })?;
    if deleted != 1 {
        return Err(invalid_data(
            "pending child tool exchange finalization is inconsistent",
        ));
    }
    Ok(())
}

fn apply_child_settlement(
    transaction: &Transaction<'_>,
    child_task_id: &ChildTaskId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    finished_at_ms: i64,
) -> StorageResult<()> {
    let AppendPurpose::ChildSettlement {
        status,
        cancel_requested,
        error,
        final_message_id,
    } = purpose
    else {
        return Err(invalid_data("child task settlement purpose is invalid"));
    };
    let changed = transaction
        .execute(
            "UPDATE child_tasks
             SET status = ?1, cancel_requested = MAX(cancel_requested, ?2), final_message_id = ?3,
                 error_code = ?4, error_message = ?5, finished_at_ms = ?6
             WHERE child_task_id = ?7 AND session_id = ?8
               AND status IN ('accepted', 'running')",
            params![
                super::mode::child_task_status_value(*status),
                i64::from(*cancel_requested),
                final_message_id
                    .as_ref()
                    .map(agent_types::MessageId::as_str),
                error
                    .as_ref()
                    .map(|error| super::run_projection::error_code_value(error.code)),
                error.as_ref().map(|error| error.message.as_str()),
                finished_at_ms,
                child_task_id.as_str(),
                session_id.as_str(),
            ],
        )
        .map_err(|source| internal_error("child task could not be settled", source))?;
    if changed != 1 {
        return Err(conflict("child task is not in a settleable state"));
    }
    Ok(())
}

fn apply_user_message_start(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    created_at_ms: i64,
) -> StorageResult<()> {
    let input_updated = transaction
        .execute(
            "UPDATE inputs
             SET state = 'committed', queued_message_json = NULL
             WHERE input_id = (SELECT input_id FROM runs WHERE run_id = ?1)
               AND session_id = ?2 AND state = 'queued'",
            params![run_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| internal_error("run input could not be committed", source))?;
    let run_updated = transaction
        .execute(
            "UPDATE runs
             SET status = 'running', started_at_ms = ?1, reasoning_effort = ?2
             WHERE run_id = ?3 AND session_id = ?4 AND status = 'accepted'",
            params![
                created_at_ms,
                reasoning_effort.map(super::mode::reasoning_effort_value),
                run_id.as_str(),
                session_id.as_str()
            ],
        )
        .map_err(|source| internal_error("run could not be started", source))?;
    if input_updated != 1 || run_updated != 1 {
        return Err(invalid_data("staged run start state is inconsistent"));
    }
    Ok(())
}

pub(super) fn apply_run_settlement(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    finished_at_ms: i64,
) -> StorageResult<()> {
    let AppendPurpose::RunSettlement {
        status,
        cancel_requested,
        error,
        goal_effect,
        proxy_report,
    } = purpose
    else {
        return Err(invalid_data("run settlement purpose is invalid"));
    };
    let updated = transaction
        .execute(
            "UPDATE runs
             SET status = ?1, cancel_requested = ?2, error_code = ?3,
                 error_message = ?4, finished_at_ms = ?5
             WHERE run_id = ?6 AND session_id = ?7
               AND status IN ('accepted', 'running', 'cancelling')",
            params![
                run_status_value(*status),
                i64::from(*cancel_requested),
                error.as_ref().map(|error| error_code_value(error.code)),
                error.as_ref().map(|error| error.message.as_str()),
                finished_at_ms,
                run_id.as_str(),
                session_id.as_str(),
            ],
        )
        .map_err(|source| internal_error("run could not be settled", source))?;
    if updated != 1 {
        return Err(conflict("run is not in a settleable state"));
    }
    if let Some(effect) = goal_effect.as_deref() {
        match effect {
            StoredGoalSettlementEffect::Continue {
                expected_goal_id,
                expected_generation,
                goal,
                next_input,
            } => {
                let binding = next_input
                    .goal_binding
                    .as_ref()
                    .ok_or_else(|| invalid_data("Goal continuation has no binding"))?;
                if goal.state != assistant_runtime::StoredGoalState::Running
                    || goal.pause_reason.is_some()
                    || goal.generation != *expected_generation
                    || goal.turn == 0
                    || next_input.session_id != *session_id
                    || next_input.origin != assistant_runtime::InputOrigin::Runtime
                    || next_input.new_goal.is_some()
                    || next_input.resumed_goal.is_some()
                    || next_input.accepted_at_ms != finished_at_ms
                    || binding.goal_id != goal.goal_id
                    || binding.generation != goal.generation
                    || binding.turn != goal.turn
                {
                    return Err(invalid_data("Goal continuation projection is invalid"));
                }
                apply_goal_settlement(
                    transaction,
                    expected_goal_id,
                    *expected_generation,
                    goal,
                    finished_at_ms,
                )?;
                insert_goal_continuation(transaction, next_input)?;
            }
            StoredGoalSettlementEffect::Transition {
                expected_goal_id,
                expected_generation,
                goal,
                ..
            } => {
                if goal.state == assistant_runtime::StoredGoalState::Running
                    || goal.generation
                        != expected_generation
                            .checked_add(1)
                            .ok_or_else(|| invalid_data("Goal generation is exhausted"))?
                {
                    return Err(invalid_data("Goal transition projection is invalid"));
                }
                apply_goal_settlement(
                    transaction,
                    expected_goal_id,
                    *expected_generation,
                    goal,
                    finished_at_ms,
                )?;
            }
        }
    }
    if let Some(report) = proxy_report.as_deref() {
        super::input_state::insert_proxy_report(transaction, session_id, run_id, *status, report)?;
    }
    let session_updated = transaction
        .execute(
            "UPDATE sessions SET updated_at_ms = ?1 WHERE session_id = ?2",
            params![finished_at_ms, session_id.as_str()],
        )
        .map_err(|source| internal_error("session activity time could not be updated", source))?;
    if session_updated != 1 {
        return Err(conflict("run session does not exist"));
    }
    Ok(())
}
