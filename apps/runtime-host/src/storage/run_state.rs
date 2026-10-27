//! Run attempt 创建、领取、终态结算和启动中断收敛。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_types::ConversationMessage;
use assistant_protocol::RunStatus;
use assistant_runtime::{
    NewStoredRunAttempt, StoredRun, StoredRunContinuation, StoredRunContinuationResult,
    StoredRunSettlement, StoredTerminalRunInputReconciliation, UserMessageCommit,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, apply_run_continuation, apply_run_settlement},
    conflict, database_write_error, internal_error,
    mode::approval_mode_value,
    recovery::AppendRequest,
};

/// 终态帧已 fsync，原提交回执已在同一事务中精确删除；只释放当前 Run 的执行控制。
/// 引用尚未闭合或删除失败会回滚本事务，原回执和控制留给正常恢复，不级联丢弃依赖。
pub(super) fn release_terminal_run_control(
    transaction: &rusqlite::Transaction<'_>,
    session_id: &assistant_protocol::SessionId,
    run_id: &assistant_protocol::RunId,
) -> StorageResult<()> {
    let input_id: Option<String> = transaction
        .query_row(
            "DELETE FROM runs WHERE run_id=?1 AND session_id=?2
         AND status IN ('completed','failed','cancelled','interrupted')
         AND NOT EXISTS(SELECT 1 FROM body_appends WHERE run_id=?1)
         AND NOT EXISTS(SELECT 1 FROM pending_tool_exchanges WHERE run_id=?1)
         AND NOT EXISTS(SELECT 1 FROM child_tasks WHERE parent_run_id=?1)
         AND NOT EXISTS(SELECT 1 FROM skill_activations WHERE run_id=?1)
         RETURNING input_id",
            params![run_id.as_str(), session_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| internal_error("terminal Run control could not be released", source))?;
    let Some(input_id) = input_id else {
        return Err(conflict(
            "terminal Run still has dependent controls or changed before release",
        ));
    };
    let shared_control: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE input_id=?1 AND session_id=?2 AND status IN ('accepted','running','cancelling'))",
        params![input_id,session_id.as_str()],|row|row.get(0),
    ).map_err(|source| internal_error("remaining Input control could not be checked",source))?;
    if shared_control {
        return Ok(());
    }
    let changed = transaction
        .execute(
            "UPDATE inputs SET priority_order=NULL,user_message_id=NULL,accepted_at_ms=NULL,
            agent_variant=NULL,origin=NULL,goal_id=NULL,goal_generation=NULL,goal_turn=NULL,
            goal_reply_route_json=NULL,skill_activation_json=NULL,cross_session_json=NULL,
            channel_source_json=NULL,agent_shell_target=NULL
         WHERE input_id=?1 AND session_id=?2 AND input_kind='message' AND state='committed'
            AND queued_message_json IS NULL
            AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.input_id=inputs.input_id)",
            params![input_id, session_id.as_str()],
        )
        .map_err(|source| internal_error("finished Input fields could not be released", source))?;
    if changed != 1 {
        return Err(conflict("finished Input still has unfinished control"));
    }
    Ok(())
}

impl StorageEngine {
    /// 目标 Session mutation 内完成队列领取；此提交前输入可撤销，提交后才存在 Run。
    /// 独立事务只保存未完成控制；后续正文/开始提交沿已有 staged append 恢复，不提前写历史。
    pub(super) fn claim_input(&mut self, run: StoredRun) -> StorageResult<()> {
        if run.status != RunStatus::Accepted
            || run.attempt != 1
            || run.started_at_ms.is_some()
            || run.finished_at_ms.is_some()
            || !run.message_ids.is_empty()
            || run.cancel_requested
        {
            return Err(conflict("invalid first Run claim"));
        }
        let input = self
            .load_input_for_id(&run.session_id, &run.input_id)?
            .ok_or_else(|| conflict("claim Input is missing"))?;
        if input.state != assistant_runtime::StoredInputState::Queued
            || input.agent_variant != run.agent_variant
        {
            return Err(conflict("claim Input is not queued"));
        }
        let history =
            self.load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: run.session_id.clone(),
            })?;
        if history
            .facts
            .runs
            .iter()
            .any(|fact| fact.run_id == run.run_id || fact.input_id.as_ref() == Some(&run.input_id))
        {
            return Err(conflict("claim Input already has Run history"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("input claim could not begin", source))?;
        let admitted: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM inputs JOIN sessions USING(session_id)
             WHERE input_id=?1 AND inputs.session_id=?2 AND state='queued' AND lifecycle='active')
             AND NOT EXISTS(SELECT 1 FROM runs WHERE input_id=?1 OR (session_id=?2 AND status IN ('accepted','running','cancelling')))",
            params![run.input_id.as_str(),run.session_id.as_str()], |row|row.get(0)
        ).map_err(|source| internal_error("input claim admission could not be checked",source))?;
        if !admitted {
            return Err(conflict("Input already claimed or Session busy"));
        }
        transaction.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,approval_mode,created_at_ms,started_at_ms) VALUES(?1,?2,?3,1,'accepted',0,?4,?5,NULL)",
            params![run.run_id.as_str(),run.session_id.as_str(),run.input_id.as_str(),approval_mode_value(run.approval_mode),run.created_at_ms]
        ).map_err(|source|database_write_error("first Run could not be claimed",source))?;
        if let Some(mut activation) = input.skill_activation {
            activation.run_id = Some(run.run_id.clone());
            let bound = transaction.execute("UPDATE skill_activations SET run_id=?1 WHERE input_id=?2 AND session_id=?3 AND run_id IS NULL",params![run.run_id.as_str(),run.input_id.as_str(),run.session_id.as_str()])
                .map_err(|source|database_write_error("claimed Skill could not be bound",source))?;
            if bound != 1 {
                return Err(conflict(
                    "claimed Skill identity is missing or already bound",
                ));
            }
            let json = serde_json::to_string(&activation)
                .map_err(|source| internal_error("claimed Skill could not be encoded", source))?;
            let bound = transaction.execute("UPDATE inputs SET skill_activation_json=?1 WHERE input_id=?2 AND session_id=?3",params![json,run.input_id.as_str(),run.session_id.as_str()])
                .map_err(|source|database_write_error("claimed Input Skill could not be bound",source))?;
            if bound != 1 {
                return Err(conflict("claimed Input identity changed"));
            }
        }
        transaction
            .commit()
            .map_err(|source| database_write_error("input claim could not commit", source))
    }

    /// 为最新的 Failed/Interrupted Run 创建同一 Input 的下一次执行尝试。
    pub(super) fn create_run_attempt(
        &mut self,
        attempt: NewStoredRunAttempt,
    ) -> StorageResult<StoredRun> {
        let history =
            self.load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: attempt.session_id.clone(),
            })?;
        let source = history
            .facts
            .runs
            .iter()
            .find(|run| run.run_id == attempt.source_run_id)
            .ok_or_else(|| conflict("source Run history does not exist"))?;
        if source.inherited
            || source.partial
            || !matches!(
                source.status,
                Some(RunStatus::Failed | RunStatus::Interrupted)
            )
        {
            return Err(conflict("Run history is not retryable"));
        }
        let input_id = source
            .input_id
            .as_ref()
            .ok_or_else(|| conflict("retry Input identity is unknown"))?;
        let input = history
            .facts
            .inputs
            .iter()
            .find(|input| &input.input_id == input_id && !input.inherited)
            .ok_or_else(|| conflict("retry Input history does not exist"))?;
        if input.goal_binding.is_some() {
            return Err(conflict("Goal Run requires resume"));
        }
        let latest = history
            .facts
            .runs
            .iter()
            .filter(|run| run.input_id.as_ref() == Some(input_id) && !run.inherited)
            .map(|run| run.attempt)
            .max()
            .ok_or_else(|| conflict("retry history is incomplete"))?;
        if source.attempt != latest
            || history
                .facts
                .runs
                .iter()
                .any(|run| run.run_id == attempt.run_id)
        {
            return Err(conflict(
                "retry history is stale or Run identity is already used",
            ));
        }
        let next = latest
            .checked_add(1)
            .ok_or_else(|| conflict("Run attempt exceeds runtime range"))?;
        let input_id = input_id.clone();
        let agent_variant = input.agent_variant;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("retry admission could not begin", source))?;
        let admitted: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM inputs JOIN sessions USING(session_id)
                WHERE input_id=?1 AND inputs.session_id=?2 AND state='committed' AND lifecycle='active' AND body_generation=?3)
             AND NOT EXISTS(SELECT 1 FROM runs WHERE input_id=?1 AND session_id=?2 AND status IN ('accepted','running','cancelling'))",
            params![input_id.as_str(), attempt.session_id.as_str(), super::to_i64(history.generation, "history generation exceeds SQLite range")?], |row| row.get(0)
        ).map_err(|source| internal_error("retry admission could not be checked", source))?;
        if !admitted {
            return Err(conflict(
                "retry Input is not committed or already has unfinished work",
            ));
        }
        // 真正的新 retry 才恢复其未完成控制所需的 Input 值；普通阅读和幂等命中不写 SQL。
        // 与 accepted Run 同事务，终结后再次释放，不将历史装配成可自行恢复的执行。
        let restored = transaction
            .execute(
                "UPDATE inputs SET user_message_id=?1,accepted_at_ms=?2,agent_variant=?3,origin=?4,
                cross_session_json=?5,channel_source_json=?6,agent_shell_target=?7,
                priority_order=NULL,goal_id=NULL,goal_generation=NULL,goal_turn=NULL,
                goal_reply_route_json=NULL,skill_activation_json=NULL
             WHERE input_id=?8 AND session_id=?9 AND state='committed' AND input_kind='message'",
                params![
                    input.user_message_id.as_str(),
                    input.accepted_at_ms,
                    super::mode::agent_variant_value(input.agent_variant),
                    super::input_state::input_origin_value(input.origin),
                    input
                        .cross_session
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|source| internal_error(
                            "retry Input binding could not be encoded",
                            source
                        ))?,
                    input
                        .channel_source
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|source| internal_error(
                            "retry Input channel could not be encoded",
                            source
                        ))?,
                    input.agent_shell_target.map(super::mode::shell_kind_value),
                    input_id.as_str(),
                    attempt.session_id.as_str()
                ],
            )
            .map_err(|source| {
                database_write_error("retry Input control could not be restored", source)
            })?;
        if restored != 1 {
            return Err(conflict("retry Input identity changed"));
        }
        transaction.execute("INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, created_at_ms, started_at_ms) VALUES (?1, ?2, ?3, ?4, 'accepted', 0, ?5, ?6, NULL)", params![attempt.run_id.as_str(), attempt.session_id.as_str(), input_id.as_str(), next, approval_mode_value(attempt.approval_mode), attempt.created_at_ms]).map_err(|source| database_write_error("run attempt could not be created", source))?;
        transaction
            .commit()
            .map_err(|source| database_write_error("retry admission could not commit", source))?;
        Ok(StoredRun {
            shell: None,
            run_id: attempt.run_id,
            session_id: attempt.session_id,
            input_id,
            attempt: next,
            status: RunStatus::Accepted,
            agent_variant,
            approval_mode: attempt.approval_mode,
            reasoning_effort: None,
            cancel_requested: false,
            error: None,
            message_ids: Vec::new(),
            message_steps: std::collections::HashMap::new(),
            created_at_ms: attempt.created_at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        })
    }

    /// 首次领取提交 User Message；后续 attempt 只把新的 Run 转为 Running。
    pub(super) fn commit_user_message(&mut self, commit: UserMessageCommit) -> StorageResult<()> {
        let reuse_committed_input = commit.message.is_none();
        let messages = commit
            .message
            .into_iter()
            .map(ConversationMessage::User)
            .collect();
        let purpose = AppendPurpose::UserMessage {
            context_management_mode: commit.context_management_mode,
            shell: commit.shell,
            reasoning_effort: commit.reasoning_effort,
            reuse_committed_input,
            input_id: Some(commit.input_id),
        };
        // 在正文写入前核验切换目标与冻结快照，避免可预知的冲突留下不可完成的 staged append。
        let preflight = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("run start preflight could not begin", source))?;
        super::append_effect::apply_user_message_start(
            &preflight,
            &commit.run_id,
            &commit.session_id,
            &purpose,
            commit.created_at_ms,
            self.allow_legacy_history,
        )?;
        preflight
            .rollback()
            .map_err(|source| internal_error("run start preflight could not roll back", source))?;

        let operation_id = commit.operation_id.clone();
        self.stage_append_for(
            AppendRequest {
                operation_id,
                session_id: commit.session_id,
                run_id: commit.run_id,
                messages,
                message_step: None,
                created_at_ms: commit.created_at_ms,
            },
            purpose,
        )?;
        self.complete_staged_append(&commit.operation_id)
    }

    /// 将 Run 与本次新增的完整规范消息作为一个可恢复操作提交。
    pub(super) fn settle_run(
        &mut self,
        settlement: StoredRunSettlement,
    ) -> StorageResult<assistant_runtime::StoredRunSettlementResult> {
        if !settlement.status.is_terminal() {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "run settlement status is not terminal",
            ));
        }
        let pending_count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pending_tool_exchanges WHERE run_id = ?1",
                [settlement.run_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|source| {
                internal_error("run pending tool exchange could not be queried", source)
            })?;
        if pending_count != 0 {
            return Err(conflict("run has a pending tool exchange"));
        }
        let goal_effect = settlement.goal_effect.clone();
        let proxy_report = settlement.proxy_report.clone();
        let mut messages = settlement.messages;
        if let Some(message) = settlement.queued_user_message.as_ref() {
            messages.insert(0, ConversationMessage::User(message.clone()));
        }
        let purpose = AppendPurpose::RunSettlement {
            status: settlement.status,
            cancel_requested: settlement.cancel_requested,
            error: settlement.error,
            queued_user_message: settlement.queued_user_message,
            goal_effect: settlement.goal_effect.map(Box::new),
            proxy_report: settlement.proxy_report,
        };
        // JSONL 先写、SQLite 后提交的 staged append 只能承受进程崩溃，不能承受一个
        // 可预知的业务 effect 校验失败。单 worker 内先在回滚事务中执行同一 effect，
        // 确认 Run/Goal CAS 与 continuation 形状均有效后才允许正文落盘。
        let preflight = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("run settlement preflight could not begin", source))?;
        apply_run_settlement(
            &preflight,
            &settlement.run_id,
            &settlement.session_id,
            &purpose,
            settlement.finished_at_ms,
            self.allow_legacy_history,
        )?;
        preflight.rollback().map_err(|source| {
            internal_error("run settlement preflight could not roll back", source)
        })?;

        let operation_id = settlement.operation_id.clone();
        self.stage_append_for(
            AppendRequest {
                operation_id,
                session_id: settlement.session_id,
                run_id: settlement.run_id,
                messages,
                message_step: settlement.message_step,
                created_at_ms: settlement.finished_at_ms,
            },
            purpose,
        )?;
        self.complete_staged_append(&settlement.operation_id)?;
        self.run_settlement_result(goal_effect.as_ref(), proxy_report.as_deref())
    }

    pub(super) fn reconcile_terminal_run_input(
        &mut self,
        reconciliation: StoredTerminalRunInputReconciliation,
    ) -> StorageResult<()> {
        if !self.allow_legacy_history {
            return Err(conflict(
                "legacy terminal Input reconciliation requires verified upgrade context",
            ));
        }
        let (run_input_id, status, input_session_id, user_message_id, state, queued_message_json): (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = self
            .connection
            .query_row(
                "SELECT runs.input_id, runs.status, inputs.session_id, inputs.user_message_id,
                        inputs.state, inputs.queued_message_json
                 FROM runs JOIN inputs ON inputs.input_id = runs.input_id
                 WHERE runs.run_id = ?1 AND runs.session_id = ?2",
                params![
                    reconciliation.run_id.as_str(),
                    reconciliation.session_id.as_str()
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(|source| internal_error("terminal run input could not be queried", source))?;
        if run_input_id != reconciliation.input_id.as_str()
            || input_session_id != reconciliation.session_id.as_str()
            || !matches!(
                status.as_str(),
                "completed" | "failed" | "cancelled" | "interrupted"
            )
            || user_message_id != reconciliation.user_message.id.as_str()
        {
            return Err(conflict(
                "terminal run input reconciliation is inconsistent",
            ));
        }
        if state == "committed" {
            if queued_message_json.is_some() {
                return Err(conflict(
                    "committed terminal run input still has a queued message",
                ));
            }
            return Ok(());
        }
        if state != "queued" {
            return Err(conflict("terminal run input state is invalid"));
        }
        let purpose = AppendPurpose::TerminalRunInputReconciliation {
            input_id: reconciliation.input_id,
            user_message: reconciliation.user_message.clone(),
        };
        let preflight = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("terminal run input reconciliation could not begin", source)
            })?;
        super::append_effect::apply_terminal_run_input_reconciliation(
            &preflight,
            &reconciliation.run_id,
            &reconciliation.session_id,
            &purpose,
            self.allow_legacy_history,
        )?;
        preflight.rollback().map_err(|source| {
            internal_error(
                "terminal run input reconciliation preflight could not roll back",
                source,
            )
        })?;
        self.stage_append_for(
            AppendRequest {
                operation_id: reconciliation.operation_id.clone(),
                session_id: reconciliation.session_id,
                run_id: reconciliation.run_id,
                messages: vec![ConversationMessage::User(reconciliation.user_message)],
                message_step: None,
                created_at_ms: reconciliation.recovered_at_ms,
            },
            purpose,
        )?;
        self.complete_staged_append(&reconciliation.operation_id)
    }

    /// 可靠追加活动 Run 的下一 Loop 上下文，并保持 Run 为 running。
    pub(super) fn commit_run_continuation(
        &mut self,
        continuation: StoredRunContinuation,
    ) -> StorageResult<StoredRunContinuationResult> {
        if continuation.messages.is_empty() {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "run continuation has no messages",
            ));
        }
        let pending_count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pending_tool_exchanges WHERE run_id = ?1",
                [continuation.run_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|source| {
                internal_error("run pending tool exchange could not be queried", source)
            })?;
        if pending_count != 0 {
            return Err(conflict("run has a pending tool exchange"));
        }
        let effect = continuation.goal_effect.clone();
        let purpose = AppendPurpose::RunContinuation {
            goal_effect: continuation.goal_effect.map(Box::new),
        };
        let preflight = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("run continuation preflight could not begin", source)
            })?;
        apply_run_continuation(
            &preflight,
            &continuation.run_id,
            &continuation.session_id,
            &purpose,
            continuation.committed_at_ms,
        )?;
        preflight.rollback().map_err(|source| {
            internal_error("run continuation preflight could not roll back", source)
        })?;
        self.stage_append_for(
            AppendRequest {
                operation_id: continuation.operation_id.clone(),
                session_id: continuation.session_id,
                run_id: continuation.run_id,
                messages: continuation.messages,
                message_step: Some(continuation.message_step),
                created_at_ms: continuation.committed_at_ms,
            },
            purpose,
        )?;
        self.complete_staged_append(&continuation.operation_id)?;
        let (goal, resume_required) = match effect {
            None => (None, false),
            Some(assistant_runtime::StoredGoalSettlementEffect::Progress { goal, .. }) => {
                (Some(goal), false)
            }
            Some(assistant_runtime::StoredGoalSettlementEffect::Transition {
                goal,
                resume_required,
                ..
            }) => (Some(goal), resume_required),
        };
        Ok(StoredRunContinuationResult {
            goal,
            resume_required,
        })
    }

    fn run_settlement_result(
        &mut self,
        effect: Option<&assistant_runtime::StoredGoalSettlementEffect>,
        proxy_report: Option<&assistant_runtime::NewStoredInput>,
    ) -> StorageResult<assistant_runtime::StoredRunSettlementResult> {
        let mut result = match effect {
            None => assistant_runtime::StoredRunSettlementResult::default(),
            Some(assistant_runtime::StoredGoalSettlementEffect::Progress { goal, .. }) => {
                assistant_runtime::StoredRunSettlementResult {
                    goal: Some(goal.clone()),
                    accepted_proxy_report: None,
                    resume_required: false,
                }
            }
            Some(assistant_runtime::StoredGoalSettlementEffect::Transition {
                goal,
                resume_required,
                ..
            }) => assistant_runtime::StoredRunSettlementResult {
                goal: Some(goal.clone()),
                accepted_proxy_report: None,
                resume_required: *resume_required,
            },
        };
        if let Some(report) = proxy_report {
            let input = self
                .load_input_for_id(&report.session_id, &report.input_id)?
                .ok_or_else(|| super::invalid_data("proxy report input is missing"))?;
            result.accepted_proxy_report = Some(assistant_runtime::AcceptedInput::Control {
                input: Box::new(input),
                run: None,
                is_duplicate: false,
            });
        }
        Ok(result)
    }
}

pub(super) fn system_time_ms() -> StorageResult<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| internal_error("system clock is before the Unix epoch", source))?;
    i64::try_from(duration.as_millis())
        .map_err(|source| internal_error("system clock exceeds SQLite range", source))
}
