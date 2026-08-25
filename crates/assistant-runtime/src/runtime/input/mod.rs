//! 单 Session 队列执行器：持久化领取、启动 Agent，并等待 Run 收敛后领取下一项。

mod commands;
mod submission;

use std::{panic::AssertUnwindSafe, sync::Arc};

use agent_core::{AgentExecution, ExecutionContext, ExecutionInput};
use agent_sdk::ContextWindowEvaluator;
use agent_types::ConversationMessage;
use assistant_protocol::{ConversationOwner, RunStatus, RuntimeErrorInfo, RuntimeEvent};
use futures_util::FutureExt;
use tokio_util::sync::CancellationToken;

use super::{
    AssistantRuntime,
    model::{RunAuthorizationInput, RunCompilationResources, compile_run_agent},
};
use crate::{
    RuntimeResult, RuntimeStore, StoredInputState, StoredRunSettlement, UserMessageCommit,
    config::ConfigRegistry,
    context_compaction::{
        MAX_AUTOMATIC_COMPACTIONS, compact_parent_context, compaction_reason_label,
        consume_execution_budget,
    },
    goal::GoalRunBinding,
    observation::ObservationCoordinator,
    run::{ActiveRun, RunModelDiagnostics, RuntimeRecorder, observe_run_execution},
    session::SessionController,
};

#[derive(Clone)]
struct QueueDriverContext {
    config_registry: Arc<ConfigRegistry>,
    permission_coordinator: Arc<crate::permission::PermissionCoordinator>,
    approval_registry: Arc<crate::permission::ApprovalRegistry>,
    model_factory: Arc<dyn crate::ModelServiceFactory>,
    run_tool_factory: Arc<dyn crate::RunToolFactory>,
    child_task_workspace_factory: Arc<dyn crate::ChildTaskWorkspaceFactory>,
    child_tasks: Arc<crate::delegation::ChildTaskRegistry>,
    context_window: Arc<ContextWindowEvaluator>,
    store: Arc<dyn RuntimeStore>,
    recall_reference_codec: Arc<crate::HmacRecallReferenceCodec>,
    events: ObservationCoordinator,
    root_cancellation: CancellationToken,
}

impl AssistantRuntime {
    pub(super) fn wake_queue(&self, session: Arc<SessionController>) -> RuntimeResult<()> {
        let should_spawn = {
            let mut state = session.lock_state()?;
            if state.is_faulted
                || state.is_queue_driver_running
                || state.next_runnable_input().is_none()
            {
                false
            } else {
                state.is_queue_driver_running = true;
                true
            }
        };
        if should_spawn {
            let context = self.queue_driver_context();
            let panic_context = context.clone();
            let panic_session = session.clone();
            self.tasks.spawn(
                run_queue(context, session),
                recover_panicked_queue(panic_context, panic_session),
            );
        }
        Ok(())
    }

    fn queue_driver_context(&self) -> QueueDriverContext {
        QueueDriverContext {
            config_registry: self.config_registry.clone(),
            permission_coordinator: self.permission_coordinator.clone(),
            approval_registry: self.approval_registry.clone(),
            model_factory: self.model_factory.clone(),
            run_tool_factory: self.run_tool_factory.clone(),
            child_task_workspace_factory: self.child_task_workspace_factory.clone(),
            child_tasks: self.child_tasks.clone(),
            context_window: self.context_window.clone(),
            store: self.store.clone(),
            recall_reference_codec: self.recall_reference_codec.clone(),
            events: self.event_sender.clone(),
            root_cancellation: self.root_cancellation.clone(),
        }
    }
}

/// Runtime 自己的队列/supervisor task 若 panic，先取消可能仍在运行的 Core，随后
/// 把无 pending 的 Run 结算为内部失败；存在持久 pending 时 fail-closed，交由重启修复。
async fn recover_panicked_queue(context: QueueDriverContext, session: Arc<SessionController>) {
    let fallback_session = session.clone();
    let recovered = AssertUnwindSafe(recover_panicked_queue_inner(context, session))
        .catch_unwind()
        .await;
    if recovered.is_err() {
        fault_driver(&fallback_session);
    }
}

async fn recover_panicked_queue_inner(
    context: QueueDriverContext,
    session: Arc<SessionController>,
) {
    let active = session.lock_state().ok().and_then(|mut state| {
        state.is_queue_driver_running = false;
        state.active_run.as_ref().map(|active| {
            active.cancellation.cancel();
            active.run_id.clone()
        })
    });
    let Some(run_id) = active else {
        fault_driver(&session);
        return;
    };
    if let Ok(settlement) =
        crate::run::settle_run(&session, &run_id, None, context.store.as_ref(), None).await
    {
        publish_settlement_events(&context.events, &session, settlement);
    }
    // Runtime supervisor 已异常退出，而 Core completion 的 observer 有意独立存在。
    // 在没有重新取得旧 Core 退出事实前，不能直接启动下一条输入；由下次启动从
    // 持久事实恢复，避免同 Session 出现两个可能回写 Recorder 的执行。
    fault_driver(&session);
}

async fn run_queue(context: QueueDriverContext, session: Arc<SessionController>) {
    loop {
        if context.root_cancellation.is_cancelled() {
            finish_driver(&session);
            return;
        }
        if session
            .ensure_conversation_loaded(context.store.as_ref())
            .await
            .is_err()
        {
            fault_driver(&session);
            return;
        }
        let mutation = session.mutation().await;
        let next = {
            let mut state = match session.lock_state() {
                Ok(state) => state,
                Err(_) => return,
            };
            if state.is_faulted || state.active_run.is_some() {
                state.is_queue_driver_running = false;
                return;
            }
            let Some(input_id) = state.next_runnable_input() else {
                state.is_queue_driver_running = false;
                return;
            };
            let Some(input) = state.inputs.get(&input_id).cloned() else {
                fault_locked_driver(&mut state);
                return;
            };
            let Some(run) = state.runs.get(&input.latest_run_id) else {
                fault_locked_driver(&mut state);
                return;
            };
            (input_id, input, run.snapshot())
        };
        let model_diagnostics = Arc::new(RunModelDiagnostics::new(
            session.id().clone(),
            next.2.run_id.clone(),
            context.events.clone(),
        ));
        let run_cancellation = context.root_cancellation.child_token();
        let start_error = match context.config_registry.snapshot().and_then(|config| {
            compile_run_agent(
                session.clone(),
                &config,
                RunCompilationResources {
                    model_factory: context.model_factory.as_ref(),
                    context_window: context.context_window.clone(),
                    run_tool_factory: context.run_tool_factory.as_ref(),
                    child_task_workspace_factory: context.child_task_workspace_factory.clone(),
                    child_tasks: context.child_tasks.clone(),
                    store: context.store.clone(),
                    recall_reference_codec: context.recall_reference_codec.clone(),
                },
                RunAuthorizationInput {
                    permission_coordinator: context.permission_coordinator.clone(),
                    approval_registry: context.approval_registry.clone(),
                    variant: next.2.variant,
                    approval_mode: next.2.approval_mode,
                    run_id: next.2.run_id.clone(),
                    cancellation: run_cancellation.clone(),
                    events: context.events.clone(),
                    goal_binding: next.1.stored.goal_binding.as_ref().map(|binding| {
                        GoalRunBinding {
                            goal_id: binding.goal_id.clone(),
                            generation: binding.generation,
                            run_id: next.2.run_id.clone(),
                        }
                    }),
                },
                Some(model_diagnostics.clone()),
            )
        }) {
            Ok(compiled) => {
                let super::model::CompiledRunParts {
                    agent,
                    authorizer,
                    compactor,
                    reasoning_effort,
                    goal_signal_latch,
                    skill_activation_latch,
                } = compiled.into_parts();
                let message = if next.1.stored.state == StoredInputState::Queued {
                    let plan = match session.lock_state() {
                        Ok(state) => state.work_plan.clone(),
                        Err(_) => {
                            fault_driver(&session);
                            return;
                        }
                    };
                    match crate::work_plan::inject_claimed_context(
                        next.1.stored.queued_message.clone(),
                        plan.as_ref(),
                    ) {
                        Ok(message) => message,
                        Err(_) => {
                            fault_driver(&session);
                            return;
                        }
                    }
                } else {
                    None
                };
                let started_at = match super::now_ms() {
                    Ok(value) => value,
                    Err(_) => {
                        fault_driver(&session);
                        return;
                    }
                };
                let operation_id = match crate::id::generate("append") {
                    Ok(value) => value,
                    Err(_) => {
                        fault_driver(&session);
                        return;
                    }
                };
                if context
                    .store
                    .commit_user_message(UserMessageCommit {
                        operation_id,
                        input_id: next.0.clone(),
                        run_id: next.2.run_id.clone(),
                        session_id: session.id().clone(),
                        message: message.clone(),
                        reasoning_effort,
                        created_at_ms: started_at,
                    })
                    .await
                    .is_err()
                {
                    fault_driver(&session);
                    return;
                }
                let (mut input, cancellation, queue_revision, generation) = {
                    let mut state = match session.lock_state() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    let queue_revision = match state.pop_runnable_input(&next.0) {
                        Some(true) => {
                            state.queue_revision = state.queue_revision.saturating_add(1);
                            Some(state.queue_revision)
                        }
                        Some(false) => None,
                        None => {
                            fault_locked_driver(&mut state);
                            return;
                        }
                    };
                    if let Some(message) = message {
                        if state
                            .journal
                            .as_mut()
                            .as_mut()
                            .and_then(|journal| {
                                journal
                                    .append_completed(ConversationMessage::User(message.clone()))
                                    .ok()
                            })
                            .is_none()
                        {
                            fault_locked_driver(&mut state);
                            return;
                        }
                        state.persisted_message_count += 1;
                        state.message_count += 1;
                        let stored =
                            &mut state.inputs.get_mut(&next.0).expect("input exists").stored;
                        stored.state = StoredInputState::Committed;
                        stored.queued_message = None;
                        state
                            .runs
                            .get_mut(&next.2.run_id)
                            .expect("run exists")
                            .extend_message_ids([message.id]);
                    }
                    let cancellation = run_cancellation;
                    state.active_run = Some(ActiveRun {
                        run_id: next.2.run_id.clone(),
                        cancellation: cancellation.clone(),
                        goal_signal_latch,
                    });
                    state
                        .runs
                        .get_mut(&next.2.run_id)
                        .expect("run exists")
                        .freeze_reasoning_effort(reasoning_effort);
                    let conversation = state
                        .journal
                        .as_ref()
                        .expect("loaded conversation")
                        .snapshot();
                    (
                        ExecutionInput { conversation },
                        cancellation,
                        queue_revision,
                        state.body_generation,
                    )
                };
                let _ = context.events.send(RuntimeEvent::ConversationCommitted {
                    owner: ConversationOwner::MainSession {
                        session_id: session.id().clone(),
                    },
                    generation,
                });
                if let Some(queue_revision) = queue_revision {
                    let _ = context.events.send(RuntimeEvent::QueueChanged {
                        session_id: session.id().clone(),
                        revision: queue_revision,
                    });
                }
                // 领取提交完成后立即释放变更门禁；supervisor 的终态结算还要再次取得同一门禁。
                drop(mutation);
                let recorder = Arc::new(RuntimeRecorder::new(
                    session.clone(),
                    next.2.run_id.clone(),
                    context.store.clone(),
                    context.events.clone(),
                    skill_activation_latch,
                ));
                let mut compaction_count = 0_u32;
                let mut remaining_budget = agent.execution_budget().clone();
                let mut next_step = std::num::NonZeroU32::MIN;
                let (outcome, forced_error) = loop {
                    let execution = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        agent.start_with_budget_at_step(
                            input.clone(),
                            ExecutionContext {
                                cancellation: cancellation.clone(),
                                recorder: recorder.clone(),
                                authorizer: authorizer.clone(),
                            },
                            remaining_budget.clone(),
                            next_step,
                        )
                    }));
                    let Ok(AgentExecution {
                        events,
                        completion,
                        control: _,
                    }) = execution
                    else {
                        break (None, None);
                    };
                    let observed = observe_run_execution(
                        session.clone(),
                        next.2.run_id.clone(),
                        events,
                        completion,
                        context.events.clone(),
                        model_diagnostics.clone(),
                    )
                    .await;
                    let (consumption, compaction_reason) = match observed.outcome.as_ref() {
                        Some(agent_core::ExecutionOutcome::CompactionRequired {
                            reason,
                            consumption,
                            ..
                        }) => (consumption, Some(*reason)),
                        Some(agent_core::ExecutionOutcome::ContinuationRequired {
                            consumption,
                            ..
                        }) => (consumption, None),
                        _ => break (observed.outcome, None),
                    };
                    consume_execution_budget(
                        &mut remaining_budget,
                        consumption.steps,
                        consumption.tool_calls,
                    );
                    let Some(advanced_step) = next_step.get().checked_add(consumption.steps) else {
                        break (
                            None,
                            Some(RuntimeErrorInfo::new(
                                assistant_protocol::RuntimeErrorCode::Internal,
                                "run step sequence overflowed",
                            )),
                        );
                    };
                    next_step = std::num::NonZeroU32::new(advanced_step)
                        .expect("a non-zero step plus consumption stays non-zero");
                    let Some(reason) = compaction_reason else {
                        input = match session.lock_state().ok().and_then(|state| {
                            state.journal.as_ref().map(|journal| ExecutionInput {
                                conversation: journal.snapshot(),
                            })
                        }) {
                            Some(input) => input,
                            None => {
                                break (
                                    None,
                                    Some(RuntimeErrorInfo::new(
                                        assistant_protocol::RuntimeErrorCode::Internal,
                                        "continued run conversation is unavailable",
                                    )),
                                );
                            }
                        };
                        continue;
                    };
                    if compaction_count >= MAX_AUTOMATIC_COMPACTIONS {
                        break (
                            None,
                            Some(RuntimeErrorInfo::new(
                                assistant_protocol::RuntimeErrorCode::ContextCompactionFailed,
                                format!(
                                    "context compaction recovery limit reached (reason={})",
                                    compaction_reason_label(reason)
                                ),
                            )),
                        );
                    }
                    compaction_count += 1;
                    match compact_parent_context(
                        compactor.as_ref(),
                        &session,
                        &next.2.run_id,
                        context.store.as_ref(),
                        cancellation.clone(),
                    )
                    .await
                    {
                        Ok(replacement) => {
                            input = ExecutionInput {
                                conversation: replacement,
                            };
                        }
                        Err(error) if error.is_cancelled() || cancellation.is_cancelled() => {
                            break (
                                Some(agent_core::ExecutionOutcome::Cancelled {
                                    consumption: agent_core::ExecutionConsumption::default(),
                                }),
                                None,
                            );
                        }
                        Err(_) => {
                            break (
                                None,
                                Some(RuntimeErrorInfo::new(
                                    assistant_protocol::RuntimeErrorCode::ContextCompactionFailed,
                                    format!(
                                        "context compaction failed (reason={})",
                                        compaction_reason_label(reason)
                                    ),
                                )),
                            );
                        }
                    }
                };
                let settled = if let Some(error) = forced_error {
                    crate::run::settle_run_with_error(
                        &session,
                        &next.2.run_id,
                        error,
                        context.store.as_ref(),
                        Some(model_diagnostics.as_ref()),
                    )
                    .await
                } else {
                    crate::run::settle_run(
                        &session,
                        &next.2.run_id,
                        outcome,
                        context.store.as_ref(),
                        Some(model_diagnostics.as_ref()),
                    )
                    .await
                };
                match settled {
                    Ok(settlement) => {
                        publish_settlement_events(&context.events, &session, settlement);
                    }
                    Err(_) => fault_driver(&session),
                }
                continue;
            }
            Err(error) => Some(error.to_protocol_info()),
        };
        fail_before_start(
            &context,
            &session,
            &next.2.run_id,
            start_error.unwrap_or_else(|| {
                RuntimeErrorInfo::new(
                    assistant_protocol::RuntimeErrorCode::Internal,
                    "agent execution could not be started",
                )
            }),
        )
        .await;
        return;
    }
}

fn publish_settlement_events(
    events: &ObservationCoordinator,
    session: &SessionController,
    settlement: crate::run::RunSettlementResult,
) {
    let committed_step = settlement.committed_step;
    if let Ok(state) = session.lock_state() {
        let generation = state.body_generation;
        drop(state);
        let _ = events.send(RuntimeEvent::ConversationCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            generation,
        });
        if let Some(step) = committed_step {
            let _ = events.send(RuntimeEvent::StepCommitted {
                owner: ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                },
                step,
                generation,
            });
        }
    }
    let _ = events.send(crate::run::finished_event(settlement.run));
    if let Some(goal) = settlement.goal {
        let _ = events.send(RuntimeEvent::GoalChanged {
            session_id: session.id().clone(),
            goal_id: goal.goal_id,
            generation: goal.generation,
        });
    }
    if let Some(continuation) = settlement.continuation {
        let _ = events.send(RuntimeEvent::RunAccepted {
            session_id: continuation.session_id,
            run_id: continuation.run_id,
        });
    }
}

async fn fail_before_start(
    context: &QueueDriverContext,
    session: &SessionController,
    run_id: &assistant_protocol::RunId,
    error: RuntimeErrorInfo,
) {
    let Ok(finished_at) = super::now_ms() else {
        fault_driver(session);
        return;
    };
    let Ok(operation_id) = crate::id::generate("append") else {
        fault_driver(session);
        return;
    };
    if context
        .store
        .settle_run(StoredRunSettlement {
            operation_id,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            status: RunStatus::Failed,
            cancel_requested: false,
            error: Some(error.clone()),
            messages: Vec::new(),
            message_step: None,
            goal_effect: None,
            finished_at_ms: finished_at,
        })
        .await
        .is_err()
    {
        fault_driver(session);
        return;
    }
    if let Ok(mut state) = session.lock_state() {
        let failed_input_id = state.runs.get(run_id).map(|run| run.input_id().clone());
        if let Some(input_id) = failed_input_id.as_ref()
            && let Some(is_user) = state.pop_runnable_input(input_id)
            && is_user
        {
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        if let Some(run) = state.runs.get_mut(run_id) {
            run.fail_before_start(error.clone(), finished_at);
        }
        state.updated_at_ms = finished_at;
        state.is_queue_driver_running = false;
    }
    let _ = context
        .events
        .send(assistant_protocol::RuntimeEvent::RunFinished {
            session_id: session.id().clone(),
            run_id: run_id.clone(),
            status: RunStatus::Failed,
            error: Some(error),
        });
}

fn finish_driver(session: &SessionController) {
    if let Ok(mut state) = session.lock_state() {
        state.is_queue_driver_running = false;
    }
}

fn fault_driver(session: &SessionController) {
    let _ = session.mark_faulted();
}

fn fault_locked_driver(state: &mut crate::session::SessionState) {
    state.is_faulted = true;
    state.is_queue_driver_running = false;
}
