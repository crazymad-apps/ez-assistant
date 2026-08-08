//! 单 Session 队列执行器：持久化领取、启动 Agent，并等待 Run 收敛后领取下一项。

mod commands;

use std::{panic::AssertUnwindSafe, sync::Arc};

use agent_core::{AgentExecution, ExecutionContext, ExecutionInput};
use agent_sdk::ContextWindowEvaluator;
use agent_tools::ToolSetSnapshot;
use agent_types::ConversationMessage;
use assistant_protocol::{RunStatus, RuntimeErrorInfo};
use futures_util::FutureExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{AssistantRuntime, model::compile_run_agent};
use crate::{
    RuntimeResult, RuntimeStore, StoredInputState, StoredRunSettlement, UserMessageCommit,
    config::ConfigRegistry,
    run::{ActiveRun, RuntimeRecorder, supervise_run},
    session::SessionController,
};

#[derive(Clone)]
struct QueueDriverContext {
    config_registry: Arc<ConfigRegistry>,
    model_factory: Arc<dyn crate::ModelServiceFactory>,
    tools: ToolSetSnapshot,
    context_window: Arc<ContextWindowEvaluator>,
    authorizer: Arc<dyn agent_core::ToolAuthorizer>,
    store: Arc<dyn RuntimeStore>,
    events: broadcast::Sender<assistant_protocol::RuntimeEvent>,
    root_cancellation: CancellationToken,
}

impl AssistantRuntime {
    pub(super) fn wake_queue(&self, session: Arc<SessionController>) -> RuntimeResult<()> {
        let should_spawn = {
            let mut state = session.lock_state()?;
            if state.is_faulted
                || (state.resume_required && state.retry_override_input.is_none())
                || state.is_queue_driver_running
                || state.runnable_inputs.is_empty()
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
            model_factory: self.model_factory.clone(),
            tools: self.tools.clone(),
            context_window: self.context_window.clone(),
            authorizer: self.default_authorizer.clone(),
            store: self.store.clone(),
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
    if let Ok(snapshot) =
        crate::run::settle_run(&session, &run_id, None, context.store.as_ref()).await
    {
        let _ = context.events.send(crate::run::finished_event(snapshot));
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
            let Some(input_id) = state.runnable_inputs.front().cloned() else {
                state.is_queue_driver_running = false;
                return;
            };
            if state.resume_required && state.retry_override_input.as_ref() != Some(&input_id) {
                state.is_queue_driver_running = false;
                return;
            }
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
        let start_error = match context.config_registry.snapshot().and_then(|config| {
            compile_run_agent(
                &session,
                &config,
                context.model_factory.as_ref(),
                context.context_window.clone(),
                context.tools.clone(),
            )
        }) {
            Ok(agent) => {
                let message = if next.1.stored.state == StoredInputState::Queued {
                    next.1.stored.queued_message.clone()
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
                        created_at_ms: started_at,
                    })
                    .await
                    .is_err()
                {
                    fault_driver(&session);
                    return;
                }
                let (input, cancellation) = {
                    let mut state = match session.lock_state() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                    state.runnable_inputs.pop_front();
                    if state.retry_override_input.as_ref() == Some(&next.0) {
                        state.retry_override_input = None;
                    }
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
                    let cancellation = context.root_cancellation.child_token();
                    state.active_run = Some(ActiveRun {
                        run_id: next.2.run_id.clone(),
                        cancellation: cancellation.clone(),
                    });
                    let conversation = state
                        .journal
                        .as_ref()
                        .expect("loaded conversation")
                        .snapshot();
                    (ExecutionInput { conversation }, cancellation)
                };
                // 领取提交完成后立即释放变更门禁；supervisor 的终态结算还要再次取得同一门禁。
                drop(mutation);
                let recorder = Arc::new(RuntimeRecorder::new(
                    session.clone(),
                    next.2.run_id.clone(),
                    context.store.clone(),
                ));
                match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    agent.start(
                        input,
                        ExecutionContext {
                            cancellation,
                            recorder,
                            authorizer: context.authorizer.clone(),
                        },
                    )
                })) {
                    Ok(AgentExecution {
                        events,
                        completion,
                        control: _,
                    }) => {
                        supervise_run(
                            session.clone(),
                            next.2.run_id.clone(),
                            events,
                            completion,
                            context.events.clone(),
                            context.store.clone(),
                        )
                        .await;
                        continue;
                    }
                    Err(_) => {
                        match crate::run::settle_run(
                            &session,
                            &next.2.run_id,
                            None,
                            context.store.as_ref(),
                        )
                        .await
                        {
                            Ok(snapshot) => {
                                let _ = context.events.send(crate::run::finished_event(snapshot));
                            }
                            Err(_) => fault_driver(&session),
                        }
                        continue;
                    }
                }
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
        if state.runnable_inputs.front() == failed_input_id.as_ref() {
            state.runnable_inputs.pop_front();
        }
        if state.retry_override_input == failed_input_id {
            state.retry_override_input = None;
        }
        if let Some(run) = state.runs.get_mut(run_id) {
            run.fail_before_start(error.clone());
        }
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
