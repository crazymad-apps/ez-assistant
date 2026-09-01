//! Core completion 到规范正文、Store 终态和内存 Run 投影的一致结算。
//!
//! 结算以 Conversation Journal 与 Store 为权威事实：先在 Journal 副本上形成候选正文及 Goal、代理报告
//! 等伴随效果，再由 Store 原子提交 Run 终态，成功后才替换 Session 内存投影。跨 Session 的主控报告
//! 必须等源 Session gate 释放后再投影；Channel 播报队列和隐藏补播提醒均不反向改变已提交的 Run 结果。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_core::{ExecutionError, ExecutionOutcome};
use agent_model::ModelError;
use agent_types::{AssistantMessage, AssistantPart, ConversationMessage};
use assistant_protocol::{RunId, RunSnapshot, RunStatus, RuntimeErrorCode, RuntimeErrorInfo};

use crate::{
    ChannelOutput, ChannelOutputDispatcher, RuntimeError, RuntimeResult, RuntimeStore, SessionRole,
    StoredGoalSettlementEffect, StoredInputState, StoredRunContinuation, StoredRunSettlement,
    goal::{GoalControl, GoalState},
    id,
    journal::InMemoryJournal,
    runtime::{
        MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE, MAX_SPEAK_TEXT_CHARS,
        controller::{ControllerToolCoordinator, ProxyReportDraft, reply_route_for_input},
        resolve_output_cycle_deliveries,
    },
    session::SessionController,
};

use super::{ModelFailureDiagnostics, RunModelDiagnostics, is_active_run};

/// Core 执行结果规范化后的候选终态，尚未表示 Store 已提交。
pub(super) struct RunSettlement {
    pub(super) status: RunStatus,
    pub(super) reasoning: Option<String>,
    pub(super) text: Option<String>,
    pub(super) error: Option<RuntimeErrorInfo>,
}

/// 一次 Run 结算完成后需要交给外层调度器继续处理的投影。
pub(crate) struct RunSettlementResult {
    /// 已由 Store 可靠提交并同步到 Session 内存状态的 Run 快照。
    pub(crate) run: RunSnapshot,
    /// 本次可靠结算后发生变化的 Goal 产品投影。
    pub(crate) goal: Option<assistant_protocol::GoalSnapshot>,
    /// Core 完整完成时已经提交正文的最终 step，供外层更新执行预算。
    pub(crate) final_message_step: Option<u32>,
    /// 输出周期最终关闭时交给 Host 的附加渠道投递。
    pub(crate) channel_output: Option<ChannelOutput>,
}

/// 同一业务 Run 启动下一次 AgentExecution 前已经可靠提交的投影。
pub(crate) struct RunContinuationResult {
    pub(crate) goal: Option<assistant_protocol::GoalSnapshot>,
    pub(crate) message_step: u32,
}

/// Run 结算所需的跨模块依赖和本次执行策略。
///
/// 它只借用 Runtime 权威服务，不持有 Session 状态；是否允许播报提醒由最外层执行编排冻结。
pub(crate) struct RunSettlementContext<'a> {
    store: &'a dyn RuntimeStore,
    controller: &'a ControllerToolCoordinator,
    output_dispatcher: &'a dyn ChannelOutputDispatcher,
    can_request_speech_reminder: bool,
    execution_steps: Option<(u32, u32)>,
}

impl<'a> RunSettlementContext<'a> {
    pub(crate) fn new(
        store: &'a dyn RuntimeStore,
        controller: &'a ControllerToolCoordinator,
        output_dispatcher: &'a dyn ChannelOutputDispatcher,
        can_request_speech_reminder: bool,
    ) -> Self {
        Self {
            store,
            controller,
            output_dispatcher,
            can_request_speech_reminder,
            execution_steps: None,
        }
    }

    /// 标记本次 AgentExecution 占用的全局 step 区间，供 Goal 门禁精确结算本次用量。
    pub(crate) fn with_execution_steps(mut self, start: u32, end: u32) -> Self {
        self.execution_steps = Some((start, end));
        self
    }
}

/// 依次执行 Goal 与播报门禁；命中时可靠提交消息并保持当前 Run 为 running。
pub(crate) async fn continue_run_if_required(
    session: &SessionController,
    run_id: &RunId,
    outcome: &ExecutionOutcome,
    execution_start_step: u32,
    message_step: u32,
    context: RunSettlementContext<'_>,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RuntimeResult<Option<RunContinuationResult>> {
    let RunSettlementContext {
        store,
        controller: _,
        output_dispatcher,
        can_request_speech_reminder,
        execution_steps: _,
    } = context;
    let committed_at_ms = system_time_ms()?;
    let mutation = session.mutation().await;
    let (mut candidate, goal_decision, reminder_draft, assistant_text) = {
        let mut state = session.lock_state()?;
        if !is_active_run(&state, run_id) {
            state.is_faulted = true;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run continuation ownership",
            });
        }
        let Some(journal) = state.journal.as_ref() else {
            state.is_faulted = true;
            return Err(RuntimeError::StorageUnavailable {
                operation: "continue run",
                source: None,
            });
        };
        if journal.has_pending() {
            state.is_faulted = true;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run continuation pending tool exchange",
            });
        }
        let journal_snapshot = journal.snapshot();
        let mut candidate =
            InMemoryJournal::from_snapshot(journal_snapshot.clone()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "run continuation conversation",
                }
            })?;
        let settlement = match outcome {
            ExecutionOutcome::Completed { message, .. } => {
                candidate
                    .append_completed(ConversationMessage::Assistant(message.clone()))
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "run continuation assistant message",
                    })?;
                completed_settlement(message)
            }
            ExecutionOutcome::Failed { error, .. } => failed_settlement(error, model_diagnostics),
            ExecutionOutcome::Cancelled { .. }
            | ExecutionOutcome::CompactionRequired { .. }
            | ExecutionOutcome::ContinuationRequired { .. } => return Ok(None),
        };
        let candidate_snapshot = candidate.snapshot();
        let new_messages = candidate_snapshot
            .messages
            .get(state.persisted_message_count..)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "run continuation persisted boundary",
            })?;
        let goal_decision =
            crate::runtime::goal::prepare_goal_run_decision(crate::runtime::goal::GoalRunFacts {
                state: &state,
                session_id: session.id(),
                run_id,
                status: settlement.status,
                conversation: &journal_snapshot,
                new_messages,
                execution_steps: (execution_start_step, message_step),
                finished_at_ms: committed_at_ms,
            })?;
        let reminder_draft = matches!(
            goal_decision,
            crate::runtime::goal::GoalRunDecision::Settle(_)
        ) && can_request_speech_reminder
            && settlement.status == RunStatus::Completed
            && state.output_cycle.as_ref().is_some_and(|cycle| {
                !cycle.has_speech && !cycle.speech_cancelled && !cycle.speech_reminder_issued
            });
        let deliveries = reminder_draft
            .then(|| resolve_output_cycle_deliveries(&state))
            .transpose()?;
        (candidate, goal_decision, deliveries, settlement.text)
    };

    let (goal_effect, speech_reminder) = match goal_decision {
        crate::runtime::goal::GoalRunDecision::Continue { effect, message } => {
            candidate
                .append_completed(ConversationMessage::User(message))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "Goal progress message",
                })?;
            (Some(effect), false)
        }
        crate::runtime::goal::GoalRunDecision::Settle(effect) => {
            let requires_speech = if let Some(deliveries) = reminder_draft {
                output_dispatcher.requires_speech(deliveries).await
            } else {
                false
            };
            if !requires_speech {
                return Ok(None);
            }
            let (message, _) =
                crate::internal_boundary::InternalBoundaryCoordinator::hidden_message(
                    crate::internal_boundary::InternalBoundaryRequest {
                        source:
                            crate::internal_boundary::InternalBoundarySource::SpeechDeliveryReminder,
                        text: speech_delivery_reminder_text(),
                    },
                )?;
            candidate
                .append_completed(ConversationMessage::User(message))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "speech delivery reminder message",
                })?;
            (effect, true)
        }
    };
    let candidate = candidate.snapshot();
    let (messages, persisted_count) = {
        let state = session.lock_state()?;
        let messages = candidate
            .messages
            .get(state.persisted_message_count..)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "run continuation persisted boundary",
            })?
            .to_vec();
        (messages, state.persisted_message_count)
    };
    let operation_id =
        id::generate("append").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "run continuation operation id",
        })?;
    let stored = store
        .commit_run_continuation(StoredRunContinuation {
            operation_id,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            messages: messages.clone(),
            message_step,
            goal_effect,
            committed_at_ms,
        })
        .await
        .map_err(|source| RuntimeError::from_store("continue run", source))?;
    let result = {
        let mut state = session.lock_state()?;
        if state.persisted_message_count != persisted_count || !is_active_run(&state, run_id) {
            state.is_faulted = true;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run continuation projection boundary",
            });
        }
        state
            .journal
            .as_mut()
            .ok_or(RuntimeError::StorageUnavailable {
                operation: "continue run",
                source: None,
            })?
            .replace_completed(candidate)
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "continued conversation projection",
            })?;
        state.persisted_message_count = state
            .journal
            .as_ref()
            .expect("journal exists after continuation")
            .message_count();
        state.message_count = state
            .message_count
            .checked_add(u64::try_from(messages.len()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "conversation message count",
                }
            })?)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "conversation message count",
            })?;
        let ids = messages.iter().map(message_id).cloned().collect::<Vec<_>>();
        state
            .runs
            .get_mut(run_id)
            .expect("active run exists")
            .extend_message_ids_at_step(ids, message_step);
        let goal = if let Some(goal) = stored.goal {
            let control = GoalControl::try_from(goal).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "continued Goal projection",
                }
            })?;
            let snapshot = crate::runtime::product::project_goal(&control)?;
            if control.state == GoalState::Completed {
                state.goal = None;
            } else {
                state.goal = Some(control);
            }
            Some(snapshot)
        } else {
            None
        };
        state.resume_required = stored.resume_required;
        state.updated_at_ms = committed_at_ms;
        if speech_reminder && let Some(cycle) = state.output_cycle.as_mut() {
            cycle.speech_reminder_issued = true;
            cycle.pending_assistant_text = assistant_text;
        }
        RunContinuationResult { goal, message_step }
    };
    drop(mutation);
    Ok(Some(result))
}

/// 先完成正文与 Run 的 Store 原子结算，再替换内存投影。
///
/// 调用时 Core execution 已经终止，但目标 Run 仍必须是 Session 当前活动 Run。函数在整个本 Session
/// 结算期间持有 mutation gate，短期状态锁不跨 Store 或 Host `await`。Store 成功前不会修改规范 Journal
/// 或 Run 内存终态；Store 失败会令 Session fail-closed。返回成功时本 Session 投影已经收敛，跨 Session
/// 代理报告也已经在不同时持有两个 Session gate 的前提下完成投影。
pub(crate) async fn settle_run(
    session: &SessionController,
    run_id: &RunId,
    outcome: Option<ExecutionOutcome>,
    context: RunSettlementContext<'_>,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RuntimeResult<RunSettlementResult> {
    settle_run_inner(session, run_id, outcome, context, model_diagnostics, None).await
}

/// Runtime 编排（例如自动压缩）失败时，以已经脱敏的稳定错误结算同一 Run。
///
/// 该入口只替换候选执行结果，不绕过 [`settle_run`] 的持久化顺序、所有权校验或失败收敛规则。
pub(crate) async fn settle_run_with_error(
    session: &SessionController,
    run_id: &RunId,
    error: RuntimeErrorInfo,
    context: RunSettlementContext<'_>,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RuntimeResult<RunSettlementResult> {
    settle_run_inner(
        session,
        run_id,
        None,
        context,
        model_diagnostics,
        Some(error),
    )
    .await
}

/// 执行 Run 结算的唯一实现。
///
/// 顺序固定为：构造只读候选及伴随效果、提交 Store、镜像本 Session 投影、释放源 mutation gate、
/// 投影跨 Session 报告。Goal 与播报续跑已经在同一 Run 的前置门禁中完成，不在终态结算创建后继 Run。
async fn settle_run_inner(
    session: &SessionController,
    run_id: &RunId,
    outcome: Option<ExecutionOutcome>,
    context: RunSettlementContext<'_>,
    model_diagnostics: Option<&RunModelDiagnostics>,
    forced_error: Option<RuntimeErrorInfo>,
) -> RuntimeResult<RunSettlementResult> {
    let RunSettlementContext {
        store,
        controller,
        output_dispatcher: _,
        can_request_speech_reminder: _,
        execution_steps,
    } = context;
    let finished_at_ms = system_time_ms()?;
    let final_step = outcome.as_ref().and_then(|outcome| match outcome {
        ExecutionOutcome::Completed { step, .. } => Some(*step),
        _ => None,
    });
    // mutation gate 串行化同 Session 的结算、输入接受和历史变更；状态锁只保护下面各段内存临界区。
    let mutation = session.mutation().await;
    let (candidate, messages, settlement, cancel_requested, goal_effect, proxy_report_draft) = {
        let mut state = session.lock_state()?;
        if !is_active_run(&state, run_id) {
            state.is_faulted = true;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run settlement ownership",
            });
        }
        let Some(record) = state.runs.get(run_id) else {
            state.is_faulted = true;
            state.active_run = None;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run settlement record",
            });
        };
        let cancel_requested = record.snapshot().cancel_requested;
        let source_input_id = record.input_id().clone();
        let Some(journal) = state.journal.as_ref() else {
            state.is_faulted = true;
            state.is_queue_driver_running = false;
            state.active_run = None;
            return Err(RuntimeError::StorageUnavailable {
                operation: "settle run",
                source: None,
            });
        };
        let (journal_snapshot, has_pending) = (journal.snapshot(), journal.has_pending());
        // 在 Journal 副本上追加最终 Assistant Message，Store 成功前不触碰权威内存 Journal。
        let mut candidate =
            InMemoryJournal::from_snapshot(journal_snapshot.clone()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "run settlement conversation",
                }
            })?;
        let mut settlement = if let Some(error) = forced_error {
            RunSettlement {
                status: RunStatus::Failed,
                reasoning: None,
                text: None,
                error: Some(error),
            }
        } else {
            match outcome {
                Some(ExecutionOutcome::Completed { message, .. }) => {
                    match candidate
                        .append_completed(ConversationMessage::Assistant(message.clone()))
                    {
                        Ok(()) => completed_settlement(&message),
                        Err(_) => {
                            state.is_faulted = true;
                            internal_failure("final assistant message could not be committed")
                        }
                    }
                }
                Some(ExecutionOutcome::Failed { error, .. }) => {
                    failed_settlement(&error, model_diagnostics)
                }
                Some(ExecutionOutcome::Cancelled { .. }) => {
                    RunSettlement::terminal(RunStatus::Cancelled)
                }
                Some(ExecutionOutcome::CompactionRequired { .. }) => {
                    RunSettlement::terminal(RunStatus::CompactionRequired)
                }
                Some(ExecutionOutcome::ContinuationRequired { .. }) => {
                    internal_failure("agent continuation escaped the run execution loop")
                }
                None => internal_failure("agent completion task terminated unexpectedly"),
            }
        };
        // 未完成 Tool Exchange 代表副作用结果未知，不能把 Core 表面终态提交为正常完成。
        if has_pending {
            state.is_faulted = true;
            settlement = internal_failure("run ended with an incomplete tool exchange");
        }
        let candidate = candidate.snapshot();
        // Store 只接收尚未持久化的尾部增量；persisted_message_count 是内存 Journal 的可靠提交边界。
        let messages = candidate
            .messages
            .get(state.persisted_message_count..)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "persisted conversation boundary",
            })?
            .to_vec();
        // Goal effect、代理报告和播报提醒在此都只是草案，不提前修改其权威投影。
        let goal_effect = if let Some((execution_start_step, execution_end_step)) = execution_steps
        {
            match crate::runtime::goal::prepare_goal_run_decision(
                crate::runtime::goal::GoalRunFacts {
                    state: &state,
                    session_id: session.id(),
                    run_id,
                    status: settlement.status,
                    conversation: &journal_snapshot,
                    new_messages: &messages,
                    execution_steps: (execution_start_step, execution_end_step),
                    finished_at_ms,
                },
            )? {
                crate::runtime::goal::GoalRunDecision::Settle(effect) => effect,
                crate::runtime::goal::GoalRunDecision::Continue { .. } => {
                    state.is_faulted = true;
                    return Err(RuntimeError::InternalStateUnavailable {
                        component: "Goal continuation escaped the run gate pipeline",
                    });
                }
            }
        } else {
            None
        };
        let source_input = state.inputs.get(&source_input_id);
        let source_goal_id = source_input
            .and_then(|input| input.stored.goal_binding.as_ref())
            .map(|binding| binding.goal_id.clone());
        let reply_route = source_input
            .map(|input| reply_route_for_input(&input.stored))
            .unwrap_or_default();
        let goal_summary = goal_effect.as_ref().map(goal_effect_summary);
        let source_queue_empty = !state
            .inputs
            .values()
            .any(|input| input.stored.state == StoredInputState::Queued);
        let proxy_report_draft = if state.role == SessionRole::Standard
            && source_queue_empty
            && matches!(
                settlement.status,
                RunStatus::Completed
                    | RunStatus::Failed
                    | RunStatus::Cancelled
                    | RunStatus::Interrupted
            ) {
            state.proxy.as_ref().map(|proxy| ProxyReportDraft {
                source_session_id: session.id().clone(),
                source_title: state.title.clone(),
                source_run_id: run_id.clone(),
                source_goal_id,
                goal_summary,
                source_run_status: settlement.status,
                controller_session_id: proxy.controller_session_id.clone(),
                final_text: settlement.text.clone(),
                error: settlement.error.clone(),
                accepted_at_ms: finished_at_ms,
                reply_route,
            })
        } else {
            None
        };
        (
            candidate,
            messages,
            settlement,
            cancel_requested,
            goal_effect,
            proxy_report_draft,
        )
    };
    let proxy_report = proxy_report_draft
        .map(|draft| controller.prepare_proxy_report(draft).map(Box::new))
        .transpose()?;

    // Run 正文、终态、Goal effect 与可选代理报告由一个 Store 业务操作共同提交。
    let operation_id =
        id::generate("append").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "storage operation id",
        })?;
    let stored_result = match store
        .settle_run(StoredRunSettlement {
            operation_id,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            status: settlement.status,
            cancel_requested,
            error: settlement.error.clone(),
            messages: messages.clone(),
            message_step: final_step,
            goal_effect,
            proxy_report,
            finished_at_ms,
        })
        .await
    {
        Ok(result) => result,
        Err(source) => {
            let mut state = session.lock_state()?;
            state.is_faulted = true;
            state.active_run = None;
            return Err(RuntimeError::from_store("settle run", source));
        }
    };

    let accepted_proxy_report = stored_result.accepted_proxy_report;
    let stored_goal = stored_result.goal;
    let resume_required = stored_result.resume_required;
    let assistant_text = settlement.text.clone();
    // Store 已是权威事实；以下只镜像返回结果，不再重新推导持久结算内容。
    let result = {
        let mut state = session.lock_state()?;
        let message_ids = messages.iter().map(message_id).cloned().collect::<Vec<_>>();
        state
            .journal
            .as_mut()
            .ok_or(RuntimeError::StorageUnavailable {
                operation: "settle run",
                source: None,
            })?
            .replace_completed(candidate)
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "settled conversation projection",
            })?;
        state.persisted_message_count = state
            .journal
            .as_ref()
            .expect("journal exists after replacement")
            .message_count();
        state.message_count = state
            .message_count
            .checked_add(u64::try_from(messages.len()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "conversation message count",
                }
            })?)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "conversation message count",
            })?;
        let record = state
            .runs
            .get_mut(run_id)
            .expect("run existence checked before persistence");
        if let Some(step) = final_step {
            record.extend_message_ids_at_step(message_ids, step);
        } else {
            record.extend_message_ids(message_ids);
        }
        record.settle(settlement, finished_at_ms);
        let snapshot = record.snapshot();
        let goal = if let Some(goal) = stored_goal {
            let control = GoalControl::try_from(goal).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "settled Goal projection",
                }
            })?;
            let snapshot = crate::runtime::product::project_goal(&control)?;
            if control.state == GoalState::Completed {
                state.goal = None;
            } else {
                state.goal = Some(control);
            }
            Some(snapshot)
        } else {
            None
        };
        state.resume_required = resume_required;
        state.updated_at_ms = finished_at_ms;
        state.active_run = None;
        let channel_output = if snapshot.status == RunStatus::Completed {
            let text = if state
                .output_cycle
                .as_ref()
                .is_some_and(|cycle| cycle.speech_reminder_issued)
            {
                state
                    .output_cycle
                    .as_ref()
                    .and_then(|cycle| cycle.pending_assistant_text.clone())
            } else {
                assistant_text
            };
            take_channel_output(&mut state, session.id(), run_id, text)
        } else if state
            .output_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.speech_reminder_issued)
        {
            let text = state
                .output_cycle
                .as_ref()
                .and_then(|cycle| cycle.pending_assistant_text.clone());
            take_channel_output(&mut state, session.id(), run_id, text)
        } else {
            // 已交给 Host 的片段可能已播放，后续失败不撤回；这里只关闭 Runtime 输出周期。
            state.output_cycle.take();
            None
        };
        RunSettlementResult {
            run: snapshot,
            goal,
            final_message_step: final_step,
            channel_output,
        }
    };
    // 报告已与源 Run 在同一 Store 事务落库；必须先释放源 Session gate，再进入主控 Session 投影。
    drop(mutation);
    if let Some(report) = accepted_proxy_report {
        controller.project_proxy_report(report).await?;
    }
    Ok(result)
}

/// 消费当前输出周期并形成一次最终附加渠道投递。
///
/// 完整 Assistant 正文已经在 Store 中，本函数只取走易失的 delivery 关联和播报完成标志；调用后同一
/// 周期不能再次结算 ChannelOutput。目标解析失败时返回 `None`，不会回滚规范 Conversation 或 Run。
fn take_channel_output(
    state: &mut crate::session::SessionState,
    session_id: &assistant_protocol::SessionId,
    run_id: &RunId,
    assistant_text: Option<String>,
) -> Option<ChannelOutput> {
    let deliveries = resolve_output_cycle_deliveries(state).ok()?;
    let cycle = state.output_cycle.take()?;
    Some(ChannelOutput {
        session_id: session_id.clone(),
        run_id: run_id.clone(),
        assistant_text,
        speech_completed: cycle.has_speech,
        deliveries,
    })
}

/// 构造轮内隐藏补播提醒，明确它不是新的用户交互，并把工具硬上限同步给模型。
fn speech_delivery_reminder_text() -> String {
    format!(
        "This is an internal speech-delivery reminder, not a new user interaction. The user-visible answer for this output cycle is already complete. Do not apologize, acknowledge this reminder, greet the user, mention a missing tool call, or restate the answer as normal assistant prose. Call the speak tool now with a concise natural spoken version of the answer. If more than {} Unicode characters are needed, call speak multiple times sequentially in playback order, splitting only at natural semantic or sentence boundaries. Use at most {} speak calls in this output cycle; compress and prioritize the content if that is not enough. Do not number or label the segments. After the final speak call, finish without additional user-facing prose.",
        MAX_SPEAK_TEXT_CHARS, MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE
    )
}

impl RunSettlement {
    fn terminal(status: RunStatus) -> Self {
        Self {
            status,
            reasoning: None,
            text: None,
            error: None,
        }
    }
}

fn message_id(message: &ConversationMessage) -> &agent_types::MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
}

/// 为跨 Session 代理报告生成有界 Goal 状态摘要，不复制 Goal 控制器或完整恢复载荷。
fn goal_effect_summary(effect: &StoredGoalSettlementEffect) -> String {
    let goal = match effect {
        StoredGoalSettlementEffect::Progress { goal, .. }
        | StoredGoalSettlementEffect::Transition { goal, .. } => goal,
    };
    let state = serde_json::to_string(&goal.state)
        .unwrap_or_else(|_| "\"unknown\"".to_owned())
        .trim_matches('"')
        .to_owned();
    let pause_reason = goal
        .pause_reason
        .as_ref()
        .map(|reason| {
            serde_json::to_string(reason).unwrap_or_else(|_| "{\"kind\":\"unknown\"}".to_owned())
        })
        .unwrap_or_else(|| "none".to_owned());
    format!(
        "state={state}, pause_reason={pause_reason}, runs={}/{}, tokens={}/{}, usage_complete={}",
        goal.budget.used_runs,
        goal.budget.max_runs,
        goal.budget.used_total_tokens,
        goal.budget.max_total_tokens,
        goal.budget.usage_complete,
    )
}

/// 从已保留在规范 Journal 中的完整 Assistant Message 派生 Run 列表摘要。
///
/// Tool Call 与 Provider State 仍只存在于规范消息；Run 投影只聚合可展示 reasoning 和 text，不建立
/// 第二份可恢复 Conversation。
fn completed_settlement(message: &AssistantMessage) -> RunSettlement {
    let mut reasoning = String::new();
    let mut text = String::new();
    for part in &message.parts {
        match part {
            AssistantPart::Reasoning(part) => reasoning.push_str(&part.text),
            AssistantPart::Text(part) => text.push_str(&part.text),
            AssistantPart::ToolCall(_) | AssistantPart::ProviderState(_) => {}
        }
    }
    RunSettlement {
        status: RunStatus::Completed,
        reasoning: Some(reasoning),
        text: Some(text),
        error: None,
    }
}

/// 把 Core 错误映射为稳定、脱敏的 Runtime 失败终态。
///
/// Provider 原始错误不会进入 Run 投影；模型失败只保留调用方能够诊断的阶段、重试次数和输出观察事实。
fn failed_settlement(
    error: &ExecutionError,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RunSettlement {
    let (code, message) = match error {
        ExecutionError::Internal => (
            RuntimeErrorCode::Internal,
            "agent execution task terminated unexpectedly".to_owned(),
        ),
        ExecutionError::Model(error) => {
            let diagnostics = model_diagnostics
                .map(|diagnostics| diagnostics.failure(error))
                .unwrap_or_else(|| fallback_model_diagnostics(error));
            (
                RuntimeErrorCode::ModelExecutionFailed,
                model_failure_message(diagnostics),
            )
        }
        ExecutionError::ContextWindow(_) => (
            RuntimeErrorCode::Internal,
            "conversation context is invalid".to_owned(),
        ),
        ExecutionError::Record(_) => (
            RuntimeErrorCode::Internal,
            "conversation could not be recorded".to_owned(),
        ),
        ExecutionError::BudgetExceeded { .. } => (
            RuntimeErrorCode::Internal,
            "execution budget was exceeded".to_owned(),
        ),
        ExecutionError::GuardrailTriggered { .. } => (
            RuntimeErrorCode::Internal,
            "execution guardrail was triggered".to_owned(),
        ),
    };
    RunSettlement {
        status: RunStatus::Failed,
        reasoning: None,
        text: None,
        error: Some(RuntimeErrorInfo::new(code, message)),
    }
}

fn fallback_model_diagnostics(error: &ModelError) -> ModelFailureDiagnostics {
    ModelFailureDiagnostics {
        kind: super::model_diagnostics::model_failure_kind(error),
        attempts: 1,
        retries: 0,
        stream_established: false,
        output_observed: false,
    }
}

fn model_failure_message(diagnostics: ModelFailureDiagnostics) -> String {
    let stage = if diagnostics.stream_established {
        "after stream establishment"
    } else {
        "before stream establishment"
    };
    format!(
        "model execution failed {stage} (kind={}, attempts={}, retries={}, output_observed={})",
        model_failure_kind_value(diagnostics.kind),
        diagnostics.attempts,
        diagnostics.retries,
        diagnostics.output_observed,
    )
}

fn model_failure_kind_value(kind: assistant_protocol::ModelFailureKind) -> &'static str {
    super::model_diagnostics::model_failure_kind_value(kind)
}

fn internal_failure(message: &'static str) -> RunSettlement {
    RunSettlement {
        status: RunStatus::Failed,
        reasoning: None,
        text: None,
        error: Some(RuntimeErrorInfo::new(RuntimeErrorCode::Internal, message)),
    }
}

fn system_time_ms() -> RuntimeResult<i64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "system clock",
        }
    })?;
    i64::try_from(duration.as_millis()).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "system clock range",
    })
}
