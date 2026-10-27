//! 单 Session 队列执行器：持久化领取、启动 Agent，并等待 Run 收敛后领取下一项。

mod commands;
pub(crate) mod projection;
mod session_command;
mod submission;

use session_command::execute_session_command;
pub(in crate::runtime) use submission::automatic_session_title;

use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::{Arc, RwLock},
};

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
    RuntimeError, RuntimeResult, RuntimeStore, StoredInputState, StoredRunSettlement,
    UserMessageCommit,
    config::ConfigRegistry,
    context_compaction::{
        AutomaticCompactionInput, MAX_AUTOMATIC_COMPACTIONS, compact_parent_context,
        compaction_reason_label, consume_execution_budget,
    },
    goal::GoalRunBinding,
    observation::ObservationCoordinator,
    run::{ActiveRun, RunModelDiagnostics, RuntimeRecorder, observe_run_execution},
    session::SessionController,
};

#[derive(Clone)]
struct QueueDriverContext {
    attachments: Arc<RwLock<BTreeMap<assistant_protocol::AttachmentId, crate::StoredAttachment>>>,
    session_loader: Arc<super::session_loading::SessionLoader>,
    workspaces: Arc<RwLock<BTreeMap<assistant_protocol::WorkspaceId, crate::StoredWorkspace>>>,
    config_registry: Arc<ConfigRegistry>,
    model_binding_gate: Arc<tokio::sync::RwLock<()>>,
    permission_coordinator: Arc<crate::permission::PermissionCoordinator>,
    approval_registry: Arc<crate::permission::ApprovalRegistry>,
    model_factory: Arc<dyn crate::ModelServiceFactory>,
    run_tool_factory: Arc<dyn crate::RunToolFactory>,
    child_task_workspace_factory: Arc<dyn crate::ChildTaskWorkspaceFactory>,
    child_tasks: Arc<crate::delegation::ChildTaskRegistry>,
    context_window: Arc<ContextWindowEvaluator>,
    store: Arc<dyn RuntimeStore>,
    skill_package_source: Arc<dyn crate::SkillPackageSource>,
    plugin: Arc<crate::plugin::PluginManager>,
    output_dispatcher: Arc<dyn crate::ChannelOutputDispatcher>,
    recall_reference_codec: Arc<crate::HmacRecallReferenceCodec>,
    mcp_registry: Arc<crate::mcp::McpRegistry>,
    mcp_management: super::mcp::McpManagement,
    mcp_config_store: Arc<crate::mcp::McpConfigStore>,
    mcp_image_materializer: Arc<dyn crate::McpImageMaterializer>,
    events: ObservationCoordinator,
    root_cancellation: CancellationToken,
    tasks: Arc<super::tasks::RuntimeTasks>,
}

/// 已提交 Input 的重试/历史重入可能尚无能力上下文；在现有活动 Run 中先可靠追加再调用模型。
/// 调用方持有 Session mutation gate，且 commit_user_message 已成功；失败交给队列 fault/recovery，
/// 不改写旧输入、不创建新 Run，Store 成功后才同步 Journal 与 Run 消息引用。
async fn persist_recovered_context(
    context: &QueueDriverContext,
    session: &SessionController,
    run_id: &assistant_protocol::RunId,
    mcp: Option<&agent_types::UserMessage>,
    shell: Option<&agent_types::UserMessage>,
) -> RuntimeResult<()> {
    let snapshot = session
        .lock_state()?
        .journal
        .as_ref()
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "recovered context journal",
        })?
        .snapshot();
    let mut messages = Vec::new();
    for message in [mcp, shell].into_iter().flatten() {
        if !crate::internal_boundary::InternalBoundaryCoordinator::context_is_current(
            &snapshot, message,
        ) {
            messages.push(ConversationMessage::User(message.clone()));
        }
    }
    if messages.is_empty() {
        return Ok(());
    }
    context
        .store
        .commit_run_continuation(crate::StoredRunContinuation {
            operation_id: crate::id::generate("append").map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "recovered context identity",
                }
            })?,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            messages: messages.clone(),
            message_step: 1,
            goal_effect: None,
            committed_at_ms: super::now_ms()?,
        })
        .await
        .map_err(|source| RuntimeError::from_store("restore run context", source))?;
    let mut state = session.lock_state()?;
    for message in messages {
        let ConversationMessage::User(user) = &message else {
            unreachable!();
        };
        let id = user.id.clone();
        state
            .journal
            .as_mut()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "recovered context projection",
            })?
            .append_completed(message)
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "recovered context projection",
            })?;
        state.persisted_message_count += 1;
        state.message_count += 1;
        state
            .runs
            .get_mut(run_id)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "recovered context run",
            })?
            .extend_message_ids_at_step([id], 1);
    }
    Ok(())
}

impl AssistantRuntime {
    pub(super) fn wake_queue(&self, session: Arc<SessionController>) -> RuntimeResult<()> {
        wake_queue_with(self.queue_driver_context(), self.tasks.clone(), session)
    }

    pub(super) fn controller_tool_coordinator(
        &self,
    ) -> Arc<super::controller::ControllerToolCoordinator> {
        controller_tool_coordinator_from(&self.queue_driver_context())
    }

    fn queue_driver_context(&self) -> QueueDriverContext {
        QueueDriverContext {
            attachments: self.attachments.clone(),
            session_loader: self.session_loader.clone(),
            workspaces: self.workspaces.clone(),
            config_registry: self.config_registry.clone(),
            model_binding_gate: self.model_binding_gate.clone(),
            permission_coordinator: self.permission_coordinator.clone(),
            approval_registry: self.approval_registry.clone(),
            model_factory: self.model_factory.clone(),
            run_tool_factory: self.run_tool_factory.clone(),
            child_task_workspace_factory: self.child_task_workspace_factory.clone(),
            child_tasks: self.child_tasks.clone(),
            context_window: self.context_window.clone(),
            store: self.store.clone(),
            skill_package_source: self.skill_package_source.clone(),
            plugin: self.plugin.clone(),
            output_dispatcher: self.output_dispatcher.clone(),
            recall_reference_codec: self.recall_reference_codec.clone(),
            mcp_registry: self.mcp_service.registry.clone(),
            mcp_management: self.mcp_management(),
            mcp_config_store: self.mcp_service.config_store.clone(),
            mcp_image_materializer: self.mcp_service.image_materializer.clone(),
            events: self.event_sender.clone(),
            root_cancellation: self.root_cancellation.clone(),
            tasks: self.tasks.clone(),
        }
    }
}

impl QueueDriverContext {
    fn title_generation_context(&self) -> super::title::TitleGenerationContext {
        super::title::TitleGenerationContext::new(
            self.config_registry.clone(),
            self.model_factory.clone(),
            self.store.clone(),
            self.events.clone(),
            self.root_cancellation.clone(),
            self.tasks.clone(),
        )
    }
}

fn controller_tool_coordinator_from(
    context: &QueueDriverContext,
) -> Arc<super::controller::ControllerToolCoordinator> {
    let wake_context = context.clone();
    let tasks = context.tasks.clone();
    Arc::new(super::controller::ControllerToolCoordinator::new(
        context.session_loader.clone(),
        context.workspaces.clone(),
        context.config_registry.clone(),
        context.store.clone(),
        context.events.clone(),
        Arc::new(move |session| wake_queue_with(wake_context.clone(), tasks.clone(), session)),
    ))
}

fn wake_queue_with(
    context: QueueDriverContext,
    tasks: Arc<super::tasks::RuntimeTasks>,
    session: Arc<SessionController>,
) -> RuntimeResult<()> {
    let should_spawn = {
        let mut state = session.lock_state()?;
        if state.is_faulted
            || state.is_queue_driver_running
            || state.active_compaction.is_some()
            || state.next_runnable_input().is_none()
        {
            false
        } else {
            state.is_queue_driver_running = true;
            true
        }
    };
    if should_spawn {
        let panic_context = context.clone();
        let panic_session = session.clone();
        tasks.spawn(
            run_queue(context, session),
            recover_panicked_queue(panic_context, panic_session),
        );
    }
    Ok(())
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
    let controller = controller_tool_coordinator_from(&context);
    if let Ok(settlement) = crate::run::settle_run(
        &session,
        &run_id,
        None,
        crate::run::RunSettlementContext::new(
            context.store.as_ref(),
            controller.as_ref(),
            context.output_dispatcher.as_ref(),
            false,
        ),
        None,
    )
    .await
    {
        publish_settlement_events(&context, &session, settlement);
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
        let (input_id, command, queue_revision) = {
            let mut state = match session.lock_state() {
                Ok(state) => state,
                Err(_) => return,
            };
            if state.is_faulted || state.active_run.is_some() || state.active_compaction.is_some() {
                state.is_queue_driver_running = false;
                return;
            }
            let Some(input_id) = state.next_runnable_input() else {
                state.is_queue_driver_running = false;
                return;
            };
            let command = state.commands.get(&input_id).cloned();
            if command.is_some() {
                state.executing_command = Some(input_id.clone());
                state.queue_revision = state.queue_revision.saturating_add(1);
            }
            (input_id, command, state.queue_revision)
        };
        if let Some(command) = command {
            let _ = context.events.send(RuntimeEvent::QueueChanged {
                session_id: session.id().clone(),
                revision: queue_revision,
            });
            // Command 持有唯一消费位，但网络刷新不占用接纳门禁；新消息仍可实时可靠入队。
            drop(mutation);
            if execute_session_command(&context, &session, command)
                .await
                .is_err()
            {
                fault_driver(&session);
                return;
            }
            continue;
        }
        // 这里只准备候选身份用于装配权限与模型；尚未写 Store/Session，也不发布 Run 事件。
        let identity_history =
            match crate::runtime::load_identity_history(context.store.as_ref(), &session).await {
                Ok(history) => history,
                Err(_) => {
                    fault_driver(&session);
                    return;
                }
            };
        let next = {
            let mut state = match session.lock_state() {
                Ok(state) => state,
                Err(_) => return,
            };
            let Some(input) = state.inputs.get(&input_id).cloned() else {
                fault_locked_driver(&mut state);
                return;
            };
            let run_id = if let Some(run_id) = &input.latest_run_id {
                let Some(run) = state.runs.get(run_id) else {
                    fault_locked_driver(&mut state);
                    return;
                };
                run.snapshot().run_id
            } else {
                match crate::run::allocate_run_id(&state, &identity_history) {
                    Ok(id) => id,
                    Err(_) => {
                        fault_locked_driver(&mut state);
                        return;
                    }
                }
            };
            (
                input_id,
                input,
                run_id,
                state.body_generation,
                state.model_selection.clone(),
                state.agent_shell_kind,
            )
        };
        let model_diagnostics = Arc::new(RunModelDiagnostics::new(
            session.id().clone(),
            next.2.clone(),
            context.events.clone(),
        ));
        let run_cancellation = context.root_cancellation.child_token();
        // 先订阅再释放门禁，避免错过取消／置顶；事件只唤醒检查，队首仍以 Session 状态为准。
        let mut preparation_events = context.events.subscribe();
        // 准备只读取配置和目录；唯一队列驱动仍保留消费权，但不阻塞入队、取消或置顶。
        drop(mutation);
        let confirmed_shell = match session.lock_state() {
            Ok(state) => state.agent_shell_environment.clone(),
            Err(_) => {
                fault_driver(&session);
                return;
            }
        };
        let preparation = async {
            let factory = context.run_tool_factory.clone();
            let target = if next.1.stored.state == StoredInputState::Queued {
                next.1.stored.agent_shell_target.or(next.5)
            } else {
                next.5
            };
            let shell = if next.1.stored.agent_shell_target.is_none() && confirmed_shell.is_some() {
                confirmed_shell
            } else {
                tokio::task::spawn_blocking(move || factory.freeze_shell(target))
                    .await
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "shell discovery task",
                    })?
                    .map_err(|source| RuntimeError::RunToolsBuildFailed { source })?
            };
            let (skill_catalog, plugin_leases) = super::skills::prepare_run_catalog(
                context.store.as_ref(),
                context.skill_package_source.as_ref(),
                context.plugin.as_ref(),
                session.environment(),
            )
            .await?;
            let config = context.config_registry.snapshot()?;
            let controller_tools = controller_tool_coordinator_from(&context);
            compile_run_agent(
                session.clone(),
                &config,
                &context.config_registry,
                RunCompilationResources {
                    attachments: context.attachments.clone(),
                    shell,
                    skill_catalog,
                    skill_package_source: context.skill_package_source.clone(),
                    model_factory: context.model_factory.clone(),
                    context_window: context.context_window.clone(),
                    run_tool_factory: context.run_tool_factory.as_ref(),
                    child_task_workspace_factory: context.child_task_workspace_factory.clone(),
                    child_tasks: context.child_tasks.clone(),
                    store: context.store.clone(),
                    recall_reference_codec: context.recall_reference_codec.clone(),
                    controller_tools,
                    output_dispatcher: context.output_dispatcher.clone(),
                    mcp_registry: context.mcp_registry.clone(),
                    mcp_management: context.mcp_management.clone(),
                    mcp_image_materializer: context.mcp_image_materializer.clone(),
                },
                RunAuthorizationInput {
                    input_id: next.0.clone(),
                    permission_coordinator: context.permission_coordinator.clone(),
                    approval_registry: context.approval_registry.clone(),
                    variant: next.1.stored.agent_variant,
                    run_id: next.2.clone(),
                    cancellation: run_cancellation.clone(),
                    events: context.events.clone(),
                    goal_binding: next.1.stored.goal_binding.as_ref().map(|binding| {
                        GoalRunBinding {
                            goal_id: binding.goal_id.clone(),
                            generation: binding.generation,
                            run_id: next.2.clone(),
                        }
                    }),
                    input_origin: next.1.stored.origin,
                    cross_session: next.1.stored.cross_session.clone(),
                },
                Some(model_diagnostics.clone()),
            )
            .await
            .map(|compiled| (compiled, plugin_leases))
        };
        // 这里尚未领取 Input 或写入正文；丢弃目录读取 future 不丢失持久化进度。
        let prepared = {
            tokio::pin!(preparation);
            loop {
                tokio::select! {
                    biased;
                    _ = context.root_cancellation.cancelled() => {
                        finish_driver(&session);
                        return;
                    }
                    event = preparation_events.recv() => {
                        let relevant = match event {
                            Ok(envelope) => matches!(envelope.event,
                                RuntimeEvent::QueueChanged { session_id, .. } if &session_id == session.id()),
                            // 观察事件允许丢失；发生 lag 时重新核对权威队首。
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                finish_driver(&session);
                                return;
                            }
                        };
                        if relevant && !session.lock_state().is_ok_and(|state|
                            state.next_runnable_input().as_ref() == Some(&next.0)
                        ) {
                            break None;
                        }
                    }
                    result = &mut preparation => break Some(result),
                }
            }
        };
        let Some(prepared) = prepared else {
            continue;
        };
        // 与设置命令保持相同顺序：配置接纳门禁 → Session mutation。
        let binding = context.model_binding_gate.read().await;
        let mutation = session.mutation().await;
        if context.root_cancellation.is_cancelled() {
            finish_driver(&session);
            return;
        }
        let still_current = session.lock_state().is_ok_and(|state| {
            !state.is_faulted
                && state.active_run.is_none()
                && state.active_compaction.is_none()
                && state.next_runnable_input().as_ref() == Some(&next.0)
                && state
                    .inputs
                    .get(&next.0)
                    .is_some_and(|input| input.latest_run_id == next.1.latest_run_id)
                && state.body_generation == next.3
                && state.model_selection == next.4
                && state.agent_shell_kind == next.5
        });
        if !still_current {
            // 取消、暂停或置顶使本次准备失效，不得为旧 Run 写入失败或用户正文。
            continue;
        }
        if next.1.latest_run_id.is_none() {
            // 装配期间仍可撤销/置顶；重新核对队首并持 mutation 后才真正领取。
            // Store 成功后安装唯一 Run 控制，再发布接纳事实；失败保留队列并封闭本 Session。
            let created_at_ms = match super::now_ms() {
                Ok(now) => now,
                Err(_) => {
                    fault_driver(&session);
                    return;
                }
            };
            let run = crate::StoredRun {
                shell: None,
                run_id: next.2.clone(),
                session_id: session.id().clone(),
                input_id: next.0.clone(),
                attempt: 1,
                status: RunStatus::Accepted,
                agent_variant: next.1.stored.agent_variant,
                approval_mode: match session.lock_state() {
                    Ok(state) => state.approval_mode,
                    Err(_) => return,
                },
                reasoning_effort: None,
                cancel_requested: false,
                error: None,
                message_ids: Vec::new(),
                message_steps: Default::default(),
                created_at_ms,
                started_at_ms: None,
                finished_at_ms: None,
            };
            if context.store.claim_input(run.clone()).await.is_err() {
                fault_driver(&session);
                return;
            }
            {
                let mut state = match session.lock_state() {
                    Ok(state) => state,
                    Err(_) => return,
                };
                state.runs.insert(
                    run.run_id.clone(),
                    crate::run::RunRecord::accepted(&run, Vec::new()),
                );
                let input = state
                    .inputs
                    .get_mut(&next.0)
                    .expect("verified queued input");
                input.first_run_id = Some(run.run_id.clone());
                input.latest_run_id = Some(run.run_id.clone());
                if let Some(activation) = input.stored.skill_activation.as_mut() {
                    activation.run_id = Some(run.run_id.clone());
                }
                for activation in state
                    .skill_activations
                    .iter_mut()
                    .filter(|activation| activation.input_id.as_ref() == Some(&next.0))
                {
                    activation.run_id = Some(run.run_id.clone());
                }
            }
            let _ = context.events.send(RuntimeEvent::RunAccepted {
                session_id: session.id().clone(),
                run_id: run.run_id,
                created_at_ms: Some(created_at_ms),
            });
        }
        let prepared = prepared.and_then(|(compiled, leases)| {
            compiled.ensure_models_current(&context.config_registry)?;
            Ok((compiled, leases))
        });
        let start_error = match prepared {
            Ok((compiled, plugin_leases)) => {
                // 目录正文已完成装配；租约只保护装配，不因目录含插件锁住整个 Run。
                drop(plugin_leases);
                let super::model::CompiledRunParts {
                    shell,
                    model_binding,
                    mut agent,
                    history_model_binding,
                    authorizer,
                    compactor,
                    reasoning_effort,
                    goal_signal_latch,
                    skill_activation_latch,
                    can_speak,
                    disclosure_context,
                    resource_projection,
                } = compiled.into_parts();
                let context_management_mode = model_binding.model.context_management_mode();
                let replacement_journal = match session
                    .prepare_journal_for_mode(context.store.as_ref(), context_management_mode)
                    .await
                {
                    Ok(journal) => journal,
                    Err(error) => {
                        fail_before_start(&context, &session, &next.2, error.to_protocol_info())
                            .await;
                        continue;
                    }
                };
                let shell_context = match shell
                    .as_ref()
                    .map(|shell| shell.context_message(&session.environment().working_directory))
                    .transpose()
                {
                    Ok(context) => context,
                    Err(_) => {
                        fault_driver(&session);
                        return;
                    }
                };
                if let Ok(mut state) = session.lock_state() {
                    state.model_binding = Some(model_binding);
                }
                let mut message = if next.1.stored.state == StoredInputState::Queued {
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
                // 首次或环境变化随用户消息一起落账；压缩恢复由 replacement 原子提交。
                if let (Some(message), Some(shell_context)) = (&mut message, &shell_context) {
                    let current = match session.lock_state() {
                        Ok(state) => state.journal.as_ref().is_some_and(|journal| {
                            crate::shell::context_is_current(&journal.snapshot(), shell_context)
                        }),
                        Err(_) => {
                            fault_driver(&session);
                            return;
                        }
                    };
                    if !current {
                        message.parts.extend(shell_context.parts.iter().cloned());
                    }
                }
                if let (Some(message), Some(disclosure)) = (&mut message, &disclosure_context) {
                    let needs_disclosure = match session.lock_state() {
                        Ok(state) => state.journal.as_ref().is_some_and(|journal| {
                            crate::mcp::needs_initial_disclosure(&journal.snapshot(), disclosure)
                        }),
                        Err(_) => {
                            fault_driver(&session);
                            return;
                        }
                    };
                    if needs_disclosure {
                        message.parts.extend(disclosure.parts.iter().cloned());
                    }
                }
                if next.1.stored.agent_shell_target.is_some()
                    && let Some(message) = &mut message
                    && crate::shell::append_switch_previous(message, next.5).is_err()
                {
                    fault_driver(&session);
                    return;
                }
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
                        context_management_mode,
                        shell: shell.clone(),
                        operation_id,
                        input_id: next.0.clone(),
                        run_id: next.2.clone(),
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
                    state.context_management_mode = Some(context_management_mode);
                    if let Some(journal) = replacement_journal {
                        state.persisted_message_count = journal.message_count();
                        state.journal = Some(journal);
                        state.context_measurements.clear();
                    }
                    state.agent_shell_environment = shell.clone();
                    if next.1.stored.state == StoredInputState::Queued
                        && let Some(target) = next.1.stored.agent_shell_target
                    {
                        state.agent_shell_kind = Some(target);
                    }
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
                            .get_mut(&next.2)
                            .expect("run exists")
                            .extend_message_ids([message.id]);
                    }
                    let cancellation = run_cancellation;
                    let starts_output_cycle = next.1.stored.origin == crate::InputOrigin::User
                        || next
                            .1
                            .stored
                            .cross_session
                            .as_ref()
                            .is_some_and(|envelope| {
                                matches!(
                                    envelope.binding,
                                    crate::CrossSessionInputBinding::ProxyReport { .. }
                                )
                            });
                    if starts_output_cycle && state.output_cycle.is_none() {
                        state.output_cycle = Some(crate::OutputCycleState {
                            source_input_id: next.0.clone(),
                            has_speech: false,
                            speech_segment_count: 0,
                            speech_cancelled: false,
                            speech_reminder_issued: false,
                            pending_assistant_text: None,
                        });
                    }
                    state.active_run = Some(ActiveRun {
                        run_id: next.2.clone(),
                        cancellation: cancellation.clone(),
                        goal_signal_latch,
                    });
                    let record = state.runs.get_mut(&next.2).expect("run exists");
                    record.freeze_execution(reasoning_effort, shell, started_at);
                    record.model_diagnostics = Some(model_diagnostics.clone());
                    let conversation = state
                        .journal
                        .as_ref()
                        .expect("loaded conversation")
                        .snapshot();
                    (
                        ExecutionInput {
                            conversation: resource_projection.apply(conversation),
                        },
                        cancellation,
                        queue_revision,
                        state.body_generation,
                    )
                };
                if next.1.stored.state == StoredInputState::Committed {
                    if persist_recovered_context(
                        &context,
                        &session,
                        &next.2,
                        disclosure_context.as_ref(),
                        shell_context.as_ref(),
                    )
                    .await
                    .is_err()
                    {
                        fault_driver(&session);
                        return;
                    }
                    input.conversation = resource_projection.apply(match session.lock_state() {
                        Ok(state) => state
                            .journal
                            .as_ref()
                            .expect("loaded conversation")
                            .snapshot(),
                        Err(_) => {
                            fault_driver(&session);
                            return;
                        }
                    });
                }
                let _ = context.events.send(RuntimeEvent::ConversationCommitted {
                    owner: ConversationOwner::MainSession {
                        session_id: session.id().clone(),
                    },
                    generation,
                });
                if next.1.stored.state == StoredInputState::Queued
                    && next.1.stored.agent_shell_target.is_some()
                    && next.1.stored.agent_shell_target != next.5
                {
                    let _ = context.events.send(RuntimeEvent::SessionChanged {
                        session_id: session.id().clone(),
                    });
                }
                if let Some(queue_revision) = queue_revision {
                    let _ = context.events.send(RuntimeEvent::QueueChanged {
                        session_id: session.id().clone(),
                        revision: queue_revision,
                    });
                }
                // 领取提交完成后立即释放变更门禁；supervisor 的终态结算还要再次取得同一门禁。
                drop(mutation);
                drop(binding);
                let recorder = Arc::new(RuntimeRecorder::new(
                    session.clone(),
                    next.2.clone(),
                    context.store.clone(),
                    context.events.clone(),
                    skill_activation_latch,
                ));
                let mut compaction_count = 0_u32;
                let mut after_compaction = false;
                let mut remaining_budget = agent.execution_budget().clone();
                let mut next_step = std::num::NonZeroU32::MIN;
                let mut final_execution_steps = None;
                let (outcome, forced_error) = loop {
                    let execution = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        let start = if std::mem::take(&mut after_compaction) {
                            agent_sdk::Agent::start_after_compaction
                        } else {
                            agent_sdk::Agent::start_with_budget_at_step
                        };
                        start(
                            &agent,
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
                        next.2.clone(),
                        events,
                        completion,
                        context.events.clone(),
                        model_diagnostics.clone(),
                        context.mcp_registry.clone(),
                    )
                    .await;
                    let (consumption, compaction_reason, compaction_request) = match observed
                        .outcome
                        .as_ref()
                    {
                        Some(agent_core::ExecutionOutcome::CompactionRequired {
                            reason,
                            consumption,
                            handoff,
                            ..
                        }) => (
                            consumption,
                            Some(*reason),
                            handoff
                                .clone()
                                .map(agent_core::CompactionHandoff::into_request),
                        ),
                        Some(agent_core::ExecutionOutcome::Cancelled { consumption })
                        | Some(agent_core::ExecutionOutcome::ContinuationRequired {
                            reason: agent_core::ContinuationReason::ReturnToHost,
                            consumption,
                        }) => {
                            final_execution_steps = Some((
                                next_step.get(),
                                next_step
                                    .get()
                                    .saturating_add(consumption.steps)
                                    .saturating_sub(1),
                            ));
                            break (observed.outcome, None);
                        }
                        Some(agent_core::ExecutionOutcome::ContinuationRequired {
                            consumption,
                            ..
                        }) => (consumption, None, None),
                        Some(agent_core::ExecutionOutcome::Completed { consumption, .. })
                        | Some(agent_core::ExecutionOutcome::Failed { consumption, .. }) => {
                            let consumption = *consumption;
                            let outcome = observed.outcome.expect("matched execution outcome");
                            consume_execution_budget(
                                &mut remaining_budget,
                                consumption.steps,
                                consumption.tool_calls,
                            );
                            let Some(advanced_step) =
                                next_step.get().checked_add(consumption.steps)
                            else {
                                break (
                                    None,
                                    Some(RuntimeErrorInfo::new(
                                        assistant_protocol::RuntimeErrorCode::Internal,
                                        "run step sequence overflowed",
                                    )),
                                );
                            };
                            let message_step = match &outcome {
                                agent_core::ExecutionOutcome::Completed { step, .. } => *step,
                                _ => advanced_step.saturating_sub(1).max(next_step.get()),
                            };
                            let execution_start_step = next_step.get();
                            final_execution_steps = Some((execution_start_step, message_step));
                            let controller = controller_tool_coordinator_from(&context);
                            match crate::run::continue_run_if_required(
                                &session,
                                &next.2,
                                &outcome,
                                execution_start_step,
                                message_step,
                                crate::run::RunSettlementContext::new(
                                    context.store.as_ref(),
                                    controller.as_ref(),
                                    context.output_dispatcher.as_ref(),
                                    can_speak,
                                ),
                                Some(model_diagnostics.as_ref()),
                            )
                            .await
                            {
                                Ok(Some(continuation)) => {
                                    let crate::run::RunContinuationResult { goal, message_step } =
                                        continuation;
                                    let Some(continued_input) =
                                        session.lock_state().ok().and_then(|state| {
                                            state.journal.as_ref().map(|journal| ExecutionInput {
                                                conversation: resource_projection
                                                    .apply(journal.snapshot()),
                                            })
                                        })
                                    else {
                                        break (
                                            None,
                                            Some(RuntimeErrorInfo::new(
                                                assistant_protocol::RuntimeErrorCode::Internal,
                                                "continued run conversation is unavailable",
                                            )),
                                        );
                                    };
                                    input = continued_input;
                                    compaction_count = 0;
                                    next_step = std::num::NonZeroU32::new(advanced_step)
                                        .expect("a non-zero step plus consumption stays non-zero");
                                    publish_continuation_events(
                                        &context,
                                        &session,
                                        goal,
                                        message_step,
                                    );
                                    continue;
                                }
                                Ok(None) => break (Some(outcome), None),
                                Err(error) => {
                                    break (None, Some(error.to_protocol_info()));
                                }
                            }
                        }
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
                        compaction_count = 0;
                        input = match session.lock_state().ok().and_then(|state| {
                            state.journal.as_ref().map(|journal| ExecutionInput {
                                conversation: resource_projection.apply(journal.snapshot()),
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
                    let Some(normal_request) = compaction_request else {
                        break (
                            None,
                            Some(RuntimeErrorInfo::new(
                                assistant_protocol::RuntimeErrorCode::ContextCompactionFailed,
                                "context compaction handoff is missing the exact live request",
                            )),
                        );
                    };
                    let made_progress =
                        crate::context_compaction::latest_model_result_id(&input.conversation)
                            != crate::context_compaction::latest_model_result_id(
                                &normal_request.conversation,
                            );
                    if made_progress {
                        compaction_count = 0;
                    }
                    if compaction_count >= MAX_AUTOMATIC_COMPACTIONS
                        || (compaction_count > 0 && !made_progress)
                    {
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
                        session.clone(),
                        &next.2,
                        AutomaticCompactionInput {
                            reason,
                            normal_request,
                        },
                        context.store.as_ref(),
                        context.events.clone(),
                        cancellation.clone(),
                    )
                    .await
                    {
                        Ok(replacement) => {
                            after_compaction = true;
                            // Store 换代可靠完成后才为同一 Run 派生新绑定；沿用一次恢复额度与预算。
                            if let Some(binding) = &history_model_binding {
                                let generation = match session.lock_state() {
                                    Ok(state) => state.body_generation,
                                    Err(_) => break (None, None),
                                };
                                let rebound = binding.compile_main(generation).and_then(|model| {
                                    agent.try_with_model(model).map_err(|source| {
                                        crate::RuntimeError::AgentBuildFailed { source }
                                    })
                                });
                                match rebound {
                                    Ok(updated) => agent = updated,
                                    Err(_) => break (None, None),
                                }
                            }
                            input = ExecutionInput {
                                conversation: resource_projection.apply(replacement),
                            };
                        }
                        Err(error)
                            if !error.is_committed()
                                && (error.is_cancelled() || cancellation.is_cancelled()) =>
                        {
                            break (
                                Some(agent_core::ExecutionOutcome::Cancelled {
                                    consumption: agent_core::ExecutionConsumption::default(),
                                }),
                                None,
                            );
                        }
                        // Store 已成功时不能把旧投影走进普通终态落盘；保留控制供进程级恢复。
                        Err(error) if error.is_committed() => {
                            fault_driver(&session);
                            return;
                        }
                        Err(error) => break (None, Some(error.to_protocol_info())),
                    }
                };
                let settled = if let Some(error) = forced_error {
                    let controller = controller_tool_coordinator_from(&context);
                    crate::run::settle_run_with_error(
                        &session,
                        &next.2,
                        error,
                        crate::run::RunSettlementContext::new(
                            context.store.as_ref(),
                            controller.as_ref(),
                            context.output_dispatcher.as_ref(),
                            can_speak,
                        ),
                        Some(model_diagnostics.as_ref()),
                    )
                    .await
                } else {
                    let controller = controller_tool_coordinator_from(&context);
                    crate::run::settle_run(
                        &session,
                        &next.2,
                        outcome,
                        final_execution_steps.map_or_else(
                            || {
                                crate::run::RunSettlementContext::new(
                                    context.store.as_ref(),
                                    controller.as_ref(),
                                    context.output_dispatcher.as_ref(),
                                    can_speak,
                                )
                            },
                            |(start, end)| {
                                crate::run::RunSettlementContext::new(
                                    context.store.as_ref(),
                                    controller.as_ref(),
                                    context.output_dispatcher.as_ref(),
                                    can_speak,
                                )
                                .with_execution_steps(start, end)
                            },
                        ),
                        Some(model_diagnostics.as_ref()),
                    )
                    .await
                };
                match settled {
                    Ok(settlement) => {
                        publish_settlement_events(&context, &session, settlement);
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
            &next.2,
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
    context: &QueueDriverContext,
    session: &Arc<SessionController>,
    settlement: crate::run::RunSettlementResult,
) {
    if let Ok(state) = session.lock_state() {
        for request in state.feedback.values().filter(|r| {
            r.run_id == settlement.run.run_id
                && r.status == assistant_protocol::FeedbackStatus::Invalidated
        }) {
            let _ = context.events.send(RuntimeEvent::FeedbackChanged {
                request: request.clone(),
            });
        }
    }
    let should_generate_title = settlement.run.status == RunStatus::Completed
        && settlement.final_message_step.is_some()
        && session.lock_state().is_ok_and(|state| {
            state.automatic_title_pending
                && state
                    .runs
                    .get(&settlement.run.run_id)
                    .and_then(|run| state.inputs.get(run.input_id()))
                    .is_some_and(|input| input.stored.origin == crate::InputOrigin::User)
        });
    let final_message_step = settlement.final_message_step;
    if let Ok(state) = session.lock_state() {
        let generation = state.body_generation;
        drop(state);
        let _ = context.events.send(RuntimeEvent::ConversationCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            generation,
        });
        if let Some(step) = final_message_step {
            let _ = context.events.send(RuntimeEvent::StepCommitted {
                owner: ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                },
                step,
                generation,
            });
        }
    }
    let _ = context.events.send(crate::run::finished_event(
        settlement.run.clone(),
        session.lock_state().is_ok_and(|state| {
            state.feedback.values().any(|request| {
                request.run_id == settlement.run.run_id
                    && request.mode == assistant_protocol::FeedbackMode::EndTurn
                    && request.status == assistant_protocol::FeedbackStatus::Pending
            })
        }),
    ));
    if let Some(goal) = settlement.goal {
        let _ = context.events.send(RuntimeEvent::GoalChanged {
            session_id: session.id().clone(),
            goal_id: goal.goal_id,
            generation: goal.generation,
        });
    }
    if let Some(output) = settlement.channel_output {
        let dispatcher = context.output_dispatcher.clone();
        context.tasks.spawn(
            async move {
                let _ = dispatcher.dispatch(output).await;
            },
            async {},
        );
    }
    if should_generate_title {
        super::title::schedule_automatic_title(context.title_generation_context(), session.clone());
    }
}

fn publish_continuation_events(
    context: &QueueDriverContext,
    session: &SessionController,
    goal: Option<assistant_protocol::GoalSnapshot>,
    message_step: u32,
) {
    if let Ok(state) = session.lock_state() {
        let generation = state.body_generation;
        drop(state);
        let _ = context.events.send(RuntimeEvent::ConversationCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            generation,
        });
        let _ = context.events.send(RuntimeEvent::StepCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            step: message_step,
            generation,
        });
    }
    if let Some(goal) = goal {
        let _ = context.events.send(RuntimeEvent::GoalChanged {
            session_id: session.id().clone(),
            goal_id: goal.goal_id,
            generation: goal.generation,
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
    let controller = controller_tool_coordinator_from(context);
    let proxy_report_draft = session.lock_state().ok().and_then(|state| {
        let source_input = state
            .runs
            .get(run_id)
            .and_then(|run| state.inputs.get(run.input_id()).map(|input| (run, input)))?;
        let queue_empty_after_settlement = !state.inputs.values().any(|input| {
            input.stored.state == StoredInputState::Queued
                && input.stored.input_id != source_input.1.stored.input_id
        });
        (state.role == crate::SessionRole::Standard && queue_empty_after_settlement)
            .then(|| {
                state
                    .proxy
                    .as_ref()
                    .map(|proxy| super::controller::ProxyReportDraft {
                        source_session_id: session.id().clone(),
                        source_title: state.title.clone(),
                        source_run_id: run_id.clone(),
                        source_goal_id: source_input
                            .1
                            .stored
                            .goal_binding
                            .as_ref()
                            .map(|binding| binding.goal_id.clone()),
                        goal_summary: None,
                        source_run_status: RunStatus::Failed,
                        controller_session_id: proxy.controller_session_id.clone(),
                        final_text: None,
                        error: Some(error.clone()),
                        accepted_at_ms: finished_at,
                        reply_route: super::controller::reply_route_for_input(
                            &source_input.1.stored,
                        ),
                    })
            })
            .flatten()
    });
    let proxy_report = if let Some(draft) = proxy_report_draft {
        controller
            .prepare_proxy_report(draft)
            .await
            .map(|input| Some(Box::new(input)))
    } else {
        Ok(None)
    };
    let Ok(proxy_report) = proxy_report else {
        fault_driver(session);
        return;
    };
    let Some((queued_user_message, target)) = session.lock_state().ok().and_then(|state| {
        let run = state.runs.get(run_id)?;
        let input = state.inputs.get(run.input_id())?;
        let queued_user_message = match input.stored.state {
            StoredInputState::Queued => Some(input.stored.queued_message.clone()?),
            StoredInputState::Committed if input.stored.queued_message.is_none() => None,
            _ => return None,
        };
        Some((
            queued_user_message,
            input
                .stored
                .agent_shell_target
                .map(|target| (target, state.agent_shell_kind)),
        ))
    }) else {
        fault_driver(session);
        return;
    };
    let messages = if let Some((target, previous_shell)) = target {
        use crate::internal_boundary::{
            InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
        };
        let result = InternalBoundaryCoordinator::visible_message(InternalBoundaryRequest {
            source: InternalBoundarySource::ShellSwitchResult,
            text: format!(
                "Agent Shell switch to {target:?} failed; the session shell binding is unchanged. {}",
                error.message
            ),
        });
        let Ok((mut message, _)) = result else {
            fault_driver(session);
            return;
        };
        if crate::shell::append_switch_previous(&mut message, previous_shell).is_err() {
            fault_driver(session);
            return;
        }
        vec![ConversationMessage::User(message)]
    } else {
        Vec::new()
    };
    let stored_result = context
        .store
        .settle_run(StoredRunSettlement {
            operation_id,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            status: RunStatus::Failed,
            cancel_requested: false,
            error: Some(error.clone()),
            queued_user_message: queued_user_message.clone(),
            messages: messages.clone(),
            message_step: None,
            goal_effect: None,
            proxy_report,
            finished_at_ms: finished_at,
        })
        .await;
    let Ok(stored_result) = stored_result else {
        fault_driver(session);
        return;
    };
    let committed_messages = queued_user_message
        .into_iter()
        .map(ConversationMessage::User)
        .chain(messages.iter().cloned())
        .collect::<Vec<_>>();
    if let Ok(mut state) = session.lock_state() {
        for message in &committed_messages {
            let Some(journal) = state.journal.as_mut() else {
                fault_locked_driver(&mut state);
                return;
            };
            if journal.append_completed(message.clone()).is_err() {
                fault_locked_driver(&mut state);
                return;
            }
            state.persisted_message_count += 1;
            state.message_count += 1;
        }
        for request in state.feedback.values_mut().filter(|request| {
            request.run_id == *run_id
                && request.status == assistant_protocol::FeedbackStatus::Pending
        }) {
            request.status = assistant_protocol::FeedbackStatus::Invalidated;
            let _ = context.events.send(RuntimeEvent::FeedbackChanged {
                request: request.clone(),
            });
        }
        let failed_input_id = state.runs.get(run_id).map(|run| run.input_id().clone());
        if let Some(input_id) = failed_input_id.as_ref()
            && let Some(input) = state.inputs.get_mut(input_id)
            && input.stored.state == StoredInputState::Queued
        {
            input.stored.state = StoredInputState::Committed;
            input.stored.queued_message = None;
        }
        if let Some(input_id) = failed_input_id.as_ref()
            && let Some(is_user) = state.pop_runnable_input(input_id)
            && is_user
        {
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        if let Some(run) = state.runs.get_mut(run_id) {
            run.extend_message_ids(
                committed_messages
                    .iter()
                    .filter_map(|message| match message {
                        ConversationMessage::User(message) => Some(message.id.clone()),
                        _ => None,
                    }),
            );
            run.fail_before_start(error.clone(), finished_at);
        }
        if state
            .output_cycle
            .as_ref()
            .is_some_and(|cycle| failed_input_id.as_ref() == Some(&cycle.source_input_id))
        {
            state.output_cycle = None;
        }
        state.updated_at_ms = finished_at;
        state.is_queue_driver_running = false;
    }
    if !committed_messages.is_empty()
        && let Ok(state) = session.lock_state()
    {
        let _ = context.events.send(RuntimeEvent::ConversationCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            generation: state.body_generation,
        });
    }
    let _ = context
        .events
        .send(assistant_protocol::RuntimeEvent::RunFinished {
            finished_at_ms: Some(finished_at),
            session_id: session.id().clone(),
            run_id: run_id.clone(),
            status: RunStatus::Failed,
            awaiting_feedback: false,
            error: Some(error),
        });
    // 源 Run 的 Store 和内存终态均已完成，且未持有源状态锁；此后才进入主控 Session 投影。
    if let Some(report) = stored_result.accepted_proxy_report
        && controller.project_proxy_report(report).await.is_err()
    {
        fault_driver(session);
    }
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
