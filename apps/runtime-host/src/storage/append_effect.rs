//! staged append 的持久业务意图及其 SQLite 提交效果。

use agent_types::UserMessage;
use assistant_protocol::{
    ChildTaskId, ChildTaskStatus, InputId, RunId, RunStatus, RuntimeErrorInfo, SessionId,
};
use assistant_runtime::{NewStoredInput, StoredGoalSettlementEffect, StoredSkillActivation};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use super::{
    StorageResult, conflict,
    goal::apply_goal_settlement,
    internal_error, invalid_data, invalid_data_with_source,
    run_projection::{error_code_value, run_status_value},
};

/// Conversation 文件写入成功后，必须在同一 SQLite 事务应用的业务转换。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AppendPurpose {
    MessageFeedback {
        change: assistant_runtime::StoredHistoryFeedback,
    },
    /// 问答终态只写入文件，原 pending 控制在最终 SQL 事务释放。
    QuestionFeedback {
        request: assistant_protocol::FeedbackRequestSnapshot,
        expected_status: Option<assistant_protocol::FeedbackStatus>,
    },
    /// 轮后回答与下一条排队输入共用原 staged 回执，输入正文不成为文件历史。
    FeedbackInput { input: Box<NewStoredInput> },
    /// 无父工具回执的 child 与主文件目录共用一次完成事务；依赖原 child 回执。
    ChildDirectory {
        child_operation_id: String,
        child: Box<assistant_runtime::StoredChildTaskHistory>,
    },
    /// 只追加规范消息，不改变 Input/Run 状态。
    Messages,
    /// 首次 User Message 或已提交 Input 的 retry 开始，可靠启动 Run。
    UserMessage {
        #[serde(default)]
        reuse_committed_input: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_id: Option<InputId>,
        #[serde(default)]
        context_management_mode: agent_model::ContextManagementMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<assistant_runtime::FrozenShellEnvironment>,
        #[serde(default)]
        reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    },
    /// 完整工具批次已写入；清除对应的临时 pending 事实。
    ToolExchange {
        receipt_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skill_activations: Vec<StoredSkillActivation>,
    },
    /// 活动 Run 追加下一次 AgentExecution 所需消息，并保持 Run 为 running。
    RunContinuation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_effect: Option<Box<StoredGoalSettlementEffect>>,
    },
    /// 最终消息已写入，结算 Run 终态。
    RunSettlement {
        status: RunStatus,
        cancel_requested: bool,
        error: Option<RuntimeErrorInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queued_user_message: Option<UserMessage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_effect: Option<Box<StoredGoalSettlementEffect>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy_report: Option<Box<NewStoredInput>>,
    },
    /// 终态 Run 不变，只提交它自己遗留的 queued Input。
    TerminalRunInputReconciliation {
        input_id: InputId,
        user_message: UserMessage,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_exchange_receipt: Option<String>,
    },
}

/// 同一 staged append 算法的两种业务目标；物理表和路径只在 Host 内部适配。
#[derive(Clone, Debug)]
pub(super) enum ConversationStorageTarget {
    /// 不创建 Run 的会话历史变更；回执的 run_id 明确为空。
    SessionMetadata { session_id: SessionId },
    Session {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
}

impl ConversationStorageTarget {
    pub(super) fn owner(&self) -> assistant_protocol::ConversationOwner {
        match self {
            Self::Session { session_id, .. } | Self::SessionMetadata { session_id } => {
                assistant_protocol::ConversationOwner::MainSession {
                    session_id: session_id.clone(),
                }
            }
            Self::ChildTask {
                session_id,
                child_task_id,
            } => assistant_protocol::ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: child_task_id.clone(),
            },
        }
    }
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
    legacy_history: bool,
) -> StorageResult<()> {
    match (purpose, target) {
        (
            AppendPurpose::MessageFeedback { .. },
            ConversationStorageTarget::SessionMetadata { .. },
        ) => Ok(()),
        (
            AppendPurpose::ChildDirectory { child, .. },
            ConversationStorageTarget::SessionMetadata { .. },
        ) if !child.inherited && child.status.is_some_and(|status| status.is_terminal()) => Ok(()),
        (
            AppendPurpose::QuestionFeedback {
                request,
                expected_status,
            },
            ConversationStorageTarget::SessionMetadata { session_id },
        ) if &request.session_id == session_id => super::feedback::apply_feedback_mutation(
            transaction,
            &assistant_runtime::FeedbackMutation {
                request: request.clone(),
                expected_status: *expected_status,
            },
            legacy_history,
        ),
        (
            AppendPurpose::FeedbackInput { input },
            ConversationStorageTarget::SessionMetadata { session_id },
        ) if &input.session_id == session_id && input.feedback_answer.is_some() => {
            super::input_state::apply_input_acceptance(transaction, input, legacy_history)
                .map(|_| ())
        }
        (AppendPurpose::Messages, _) => Ok(()),
        (
            AppendPurpose::UserMessage { .. },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_user_message_start(
            transaction,
            run_id,
            session_id,
            purpose,
            created_at_ms,
            legacy_history,
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
            if legacy_history {
                for activation in skill_activations {
                    super::skill::insert_skill_activation(transaction, activation)?;
                }
            }
            Ok(())
        }
        (
            AppendPurpose::RunSettlement { .. },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_run_settlement(
            transaction,
            run_id,
            session_id,
            purpose,
            created_at_ms,
            legacy_history,
        ),
        (
            AppendPurpose::TerminalRunInputReconciliation { .. },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_terminal_run_input_reconciliation(
            transaction,
            run_id,
            session_id,
            purpose,
            legacy_history,
        ),
        (
            AppendPurpose::RunContinuation { .. },
            ConversationStorageTarget::Session { session_id, run_id },
        ) => apply_run_continuation(transaction, run_id, session_id, purpose, created_at_ms),
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
            if legacy_history {
                for activation in skill_activations {
                    super::skill::insert_skill_activation(transaction, activation)?;
                }
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
            legacy_history,
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
    legacy_history: bool,
) -> StorageResult<()> {
    let AppendPurpose::ChildSettlement {
        status,
        cancel_requested,
        error,
        final_message_id,
        parent_exchange_receipt,
    } = purpose
    else {
        return Err(invalid_data("child task settlement purpose is invalid"));
    };
    let changed = if legacy_history {
        transaction.execute(
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
    } else {
        transaction.execute(
            "UPDATE child_tasks SET status=?1,cancel_requested=MAX(cancel_requested,?2)
             WHERE child_task_id=?3 AND session_id=?4 AND status IN ('accepted','running')",
            params![
                super::mode::child_task_status_value(*status),
                i64::from(*cancel_requested),
                child_task_id.as_str(),
                session_id.as_str()
            ],
        )
    }
    .map_err(|source| internal_error("child task could not be settled", source))?;
    if changed != 1 {
        return Err(conflict("child task is not in a settleable state"));
    }
    if let Some(receipt) = parent_exchange_receipt {
        super::tool_exchange::freeze_child_for_pending(
            transaction,
            receipt,
            session_id,
            child_task_id,
        )?;
    }
    Ok(())
}

pub(super) fn apply_user_message_start(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    created_at_ms: i64,
    legacy_history: bool,
) -> StorageResult<()> {
    let AppendPurpose::UserMessage {
        reasoning_effort,
        context_management_mode,
        shell,
        reuse_committed_input,
        input_id,
    } = purpose
    else {
        return Err(invalid_data("run start purpose is invalid"));
    };
    if let Some(input_id) = input_id {
        let matches: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND session_id=?2 AND input_id=?3)",
            params![run_id.as_str(), session_id.as_str(), input_id.as_str()], |row| row.get(0),
        ).map_err(|source| internal_error("run input identity could not be checked", source))?;
        if !matches {
            return Err(conflict("run input does not match start"));
        }
    }
    let shell = shell.as_ref();
    // 新 Run 领取时与输入原子接纳当前机制；已运行的 Run 不经过本入口。
    let changed = transaction.execute(
        "UPDATE sessions SET context_management_mode=?1 WHERE session_id=?2 AND lifecycle='active'",
        params![super::mode::context_management_mode_value(*context_management_mode), session_id.as_str()],
    ).map_err(|source| internal_error("session context mode could not be bound", source))?;
    if changed != 1 {
        return Err(conflict("session cannot accept context management mode"));
    }

    if *reuse_committed_input {
        let changed = transaction.execute(
            "UPDATE runs SET status='running', started_at_ms=?1, reasoning_effort=?2, shell_snapshot_json=?3
             WHERE run_id=?4 AND session_id=?5 AND status='accepted'
               AND EXISTS(SELECT 1 FROM inputs WHERE input_id=runs.input_id AND session_id=?5 AND state='committed')",
            params![created_at_ms, reasoning_effort.map(super::mode::reasoning_effort_value),
                shell.map(serde_json::to_string).transpose().map_err(|source| internal_error("shell snapshot could not be encoded", source))?,
                run_id.as_str(), session_id.as_str()],
        ).map_err(|source| internal_error("retry run could not be started", source))?;
        if changed != 1 {
            return Err(conflict("retry run cannot be started"));
        }
        if !legacy_history {
            release_consumed_input_selections(transaction, run_id, session_id)?;
        }
        return Ok(());
    }

    let target: Option<String> = transaction.query_row(
        "SELECT agent_shell_target FROM inputs WHERE input_id = (SELECT input_id FROM runs WHERE run_id = ?1) AND session_id = ?2",
        params![run_id.as_str(), session_id.as_str()],
        |row| row.get(0),
    ).map_err(|source| internal_error("shell switch intent could not be read", source))?;
    if let Some(target) = super::mode::parse_shell_kind(target)? {
        if shell.map(|shell| shell.kind) != Some(target) {
            return Err(conflict(
                "shell switch snapshot does not match the queued target",
            ));
        }
        let changed = transaction.execute(
            "UPDATE sessions SET agent_shell_kind = ?1 WHERE session_id = ?2 AND lifecycle = 'active'",
            params![super::mode::shell_kind_value(target), session_id.as_str()],
        ).map_err(|source| internal_error("session shell binding could not be changed", source))?;
        if changed != 1 {
            return Err(conflict("shell switch session is not active"));
        }
    }
    transaction
        .execute(
        "UPDATE sessions SET agent_shell_environment_json=?1 WHERE session_id=?2 AND agent_shell_environment_json IS NOT ?1",
            params![
                super::agent_shell::encode_environment(shell)?,
                session_id.as_str()
            ],
        )
        .map_err(|source| internal_error("session shell environment could not be saved", source))?;
    let input_updated = transaction
        .execute(
            "UPDATE inputs
             SET state = 'committed', queued_message_json = NULL, priority_order = NULL
             WHERE input_id = (SELECT input_id FROM runs WHERE run_id = ?1)
               AND session_id = ?2 AND state = 'queued'",
            params![run_id.as_str(), session_id.as_str()],
        )
        .map_err(|source| internal_error("run input could not be committed", source))?;
    let run_updated = transaction
        .execute(
            "UPDATE runs
           SET status = 'running', started_at_ms = ?1, reasoning_effort = ?2, shell_snapshot_json = ?5
             WHERE run_id = ?3 AND session_id = ?4 AND status = 'accepted'",
            params![
                created_at_ms,
                reasoning_effort.map(super::mode::reasoning_effort_value),
                run_id.as_str(),
                session_id.as_str(),
                shell.map(serde_json::to_string).transpose().map_err(|source| internal_error("shell snapshot could not be encoded", source))?
            ],
        )
        .map_err(|source| internal_error("run could not be started", source))?;
    if input_updated != 1 || run_updated != 1 {
        return Err(invalid_data("staged run start state is inconsistent"));
    }
    if !legacy_history {
        release_consumed_input_selections(transaction, run_id, session_id)?;
    }
    Ok(())
}

pub(super) fn apply_run_settlement(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    finished_at_ms: i64,
    legacy_history: bool,
) -> StorageResult<()> {
    let AppendPurpose::RunSettlement {
        status,
        cancel_requested,
        error,
        queued_user_message,
        goal_effect,
        proxy_report,
    } = purpose
    else {
        return Err(invalid_data("run settlement purpose is invalid"));
    };
    if !legacy_history
        && transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM child_tasks WHERE parent_run_id=?1)",
                [run_id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|source| internal_error("Run child controls could not be checked", source))?
    {
        return Err(conflict("Run still has unfinished child controls"));
    }
    commit_settlement_input(
        transaction,
        run_id,
        session_id,
        queued_user_message.as_ref(),
    )?;
    if !legacy_history {
        release_consumed_input_selections(transaction, run_id, session_id)?;
    }
    let feedback_sql = if legacy_history {
        "UPDATE session_feedback SET snapshot_json=json_set(snapshot_json,'$.status','invalidated')
         WHERE run_id=?1 AND session_id=?2 AND json_extract(snapshot_json,'$.status')='pending'
         AND (json_extract(snapshot_json,'$.mode')='wait' OR ?3 != 'completed')"
    } else {
        "DELETE FROM session_feedback WHERE run_id=?1 AND session_id=?2
         AND json_extract(snapshot_json,'$.status')='pending'
         AND (json_extract(snapshot_json,'$.mode')='wait' OR ?3 != 'completed')"
    };
    transaction
        .execute(
            feedback_sql,
            params![
                run_id.as_str(),
                session_id.as_str(),
                run_status_value(*status)
            ],
        )
        .map_err(|source| internal_error("feedback settlement failed", source))?;
    let updated = if legacy_history {
        transaction.execute(
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
    } else {
        transaction.execute(
            "UPDATE runs SET status=?1,cancel_requested=?2
             WHERE run_id=?3 AND session_id=?4 AND status IN ('accepted','running','cancelling')",
            params![
                run_status_value(*status),
                i64::from(*cancel_requested),
                run_id.as_str(),
                session_id.as_str()
            ],
        )
    }
    .map_err(|source| internal_error("run could not be settled", source))?;
    if updated != 1 {
        return Err(conflict("run is not in a settleable state"));
    }
    apply_goal_effect(
        transaction,
        session_id,
        goal_effect.as_deref(),
        finished_at_ms,
    )?;
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

fn commit_settlement_input(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    queued_user_message: Option<&UserMessage>,
) -> StorageResult<()> {
    let (input_id, input_session_id, user_message_id, state, queued_message_json): (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
    ) = transaction
        .query_row(
            "SELECT inputs.input_id, inputs.session_id, inputs.user_message_id,
                    inputs.state, inputs.queued_message_json
             FROM runs JOIN inputs ON inputs.input_id = runs.input_id
             WHERE runs.run_id = ?1 AND runs.session_id = ?2",
            params![run_id.as_str(), session_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|source| internal_error("run input could not be queried", source))?;
    if input_session_id != session_id.as_str() {
        return Err(conflict("run input belongs to a different session"));
    }
    match state.as_str() {
        "committed" => {
            if queued_user_message.is_some() || queued_message_json.is_some() {
                return Err(conflict("committed run input settlement is inconsistent"));
            }
        }
        "queued" => {
            let message = queued_user_message
                .ok_or_else(|| conflict("queued run input requires its original user message"))?;
            if Some(message.id.as_str()) != user_message_id.as_deref() {
                return Err(conflict("queued run input message identity does not match"));
            }
            let stored = queued_message_json
                .as_deref()
                .ok_or_else(|| conflict("queued run input has no stored user message"))?;
            let stored: UserMessage = serde_json::from_str(stored).map_err(|source| {
                invalid_data_with_source("queued run input message is invalid", source)
            })?;
            if stored != *message {
                return Err(conflict("queued run input message does not match"));
            }
            let updated = transaction
                .execute(
                    "UPDATE inputs SET state = 'committed', queued_message_json = NULL, priority_order = NULL
                     WHERE input_id = ?1 AND session_id = ?2 AND user_message_id = ?3
                       AND state = 'queued'",
                    params![input_id, session_id.as_str(), user_message_id],
                )
                .map_err(|source| {
                    internal_error("queued run input could not be committed", source)
                })?;
            if updated != 1 {
                return Err(conflict("queued run input changed during settlement"));
            }
        }
        _ => return Err(invalid_data("run input state is invalid")),
    }
    Ok(())
}

pub(super) fn apply_terminal_run_input_reconciliation(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    legacy_history: bool,
) -> StorageResult<()> {
    let AppendPurpose::TerminalRunInputReconciliation {
        input_id,
        user_message,
    } = purpose
    else {
        return Err(invalid_data(
            "terminal run input reconciliation purpose is invalid",
        ));
    };
    let (run_input_id, status): (String, String) = transaction
        .query_row(
            "SELECT input_id, status FROM runs WHERE run_id = ?1 AND session_id = ?2",
            params![run_id.as_str(), session_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|source| internal_error("terminal run could not be queried", source))?;
    if run_input_id != input_id.as_str()
        || !matches!(
            status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        )
    {
        return Err(conflict(
            "terminal run input reconciliation does not match the run",
        ));
    }
    let (input_session_id, user_message_id, state, queued_message_json): (
        String,
        String,
        String,
        Option<String>,
    ) = transaction
        .query_row(
            "SELECT session_id, user_message_id, state, queued_message_json
             FROM inputs WHERE input_id = ?1",
            [input_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|source| internal_error("terminal run input could not be queried", source))?;
    if input_session_id != session_id.as_str()
        || state != "queued"
        || user_message.id.as_str() != user_message_id
    {
        return Err(conflict(
            "terminal run queued input reconciliation is inconsistent",
        ));
    }
    let stored = queued_message_json
        .as_deref()
        .ok_or_else(|| conflict("terminal run queued input has no stored message"))?;
    let stored: UserMessage = serde_json::from_str(stored).map_err(|source| {
        invalid_data_with_source("terminal run queued input message is invalid", source)
    })?;
    if stored != *user_message {
        return Err(conflict("terminal run queued input message does not match"));
    }
    let updated = transaction
        .execute(
            "UPDATE inputs SET state = 'committed', queued_message_json = NULL, priority_order = NULL
             WHERE input_id = ?1 AND session_id = ?2 AND user_message_id = ?3
               AND state = 'queued'",
            params![input_id.as_str(), session_id.as_str(), user_message_id],
        )
        .map_err(|source| {
            internal_error("terminal run queued input could not be committed", source)
        })?;
    if updated != 1 {
        return Err(conflict(
            "terminal run queued input changed during reconciliation",
        ));
    }
    if !legacy_history {
        release_consumed_input_selections(transaction, run_id, session_id)?;
    }
    Ok(())
}

/// 对应 User 与选择事实已冻结在同一 staged 帧中，预检仅回滚验证，最终调用发生于文件同步后。
/// 只释放本次 Run 已消费的选择；任一删除失败由调用方事务保留原队列与回执，恢复不重复消费。
fn release_consumed_input_selections(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
) -> StorageResult<()> {
    let input_id: String = transaction
        .query_row(
            "SELECT i.input_id FROM inputs i JOIN runs r ON r.input_id=i.input_id
         WHERE r.run_id=?1 AND r.session_id=?2 AND i.session_id=?2 AND i.state='committed'",
            params![run_id.as_str(), session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|source| internal_error("consumed input identity could not be checked", source))?;
    for statement in [
        "DELETE FROM skill_activations WHERE input_id=?1 AND session_id=?2",
        "DELETE FROM mcp_input_selections WHERE input_id=?1 AND session_id=?2",
        "UPDATE inputs SET skill_activation_json=NULL WHERE input_id=?1 AND session_id=?2 AND state='committed'",
    ] {
        transaction
            .execute(statement, params![input_id, session_id.as_str()])
            .map_err(|source| {
                internal_error("consumed input selection could not be released", source)
            })?;
    }
    Ok(())
}

pub(super) fn apply_run_continuation(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    session_id: &SessionId,
    purpose: &AppendPurpose,
    committed_at_ms: i64,
) -> StorageResult<()> {
    let AppendPurpose::RunContinuation { goal_effect } = purpose else {
        return Err(invalid_data("run continuation purpose is invalid"));
    };
    let running: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE run_id = ?1 AND session_id = ?2 AND status = 'running'",
            params![run_id.as_str(), session_id.as_str()],
            |row| row.get(0),
        )
        .map_err(|source| internal_error("active run could not be queried", source))?;
    if running != 1 {
        return Err(conflict("run is not active"));
    }
    apply_goal_effect(
        transaction,
        session_id,
        goal_effect.as_deref(),
        committed_at_ms,
    )?;
    let updated = transaction
        .execute(
            "UPDATE sessions SET updated_at_ms = ?1 WHERE session_id = ?2",
            params![committed_at_ms, session_id.as_str()],
        )
        .map_err(|source| internal_error("session activity time could not be updated", source))?;
    if updated != 1 {
        return Err(conflict("run session does not exist"));
    }
    Ok(())
}

fn apply_goal_effect(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    effect: Option<&StoredGoalSettlementEffect>,
    changed_at_ms: i64,
) -> StorageResult<()> {
    let Some(effect) = effect else {
        return Ok(());
    };
    match effect {
        StoredGoalSettlementEffect::Progress {
            expected_goal_id,
            expected_generation,
            goal,
        } => {
            if goal.session_id != *session_id
                || goal.state != assistant_runtime::StoredGoalState::Running
                || goal.pause_reason.is_some()
                || goal.generation != *expected_generation
                || goal.turn == 0
            {
                return Err(invalid_data("Goal progress projection is invalid"));
            }
            apply_goal_settlement(
                transaction,
                expected_goal_id,
                *expected_generation,
                goal,
                changed_at_ms,
            )
        }
        StoredGoalSettlementEffect::Transition {
            expected_goal_id,
            expected_generation,
            goal,
            ..
        } => {
            if goal.session_id != *session_id
                || goal.state == assistant_runtime::StoredGoalState::Running
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
                changed_at_ms,
            )
        }
    }
}
