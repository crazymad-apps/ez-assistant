//! 单个同步子任务的 Runtime 控制器。

use std::sync::{Arc, Mutex};

use agent_core::{ExecutionContext, ExecutionInput, ToolPolicy};
use agent_sdk::Agent;
use agent_tools::ToolError;
use agent_types::{ConversationMessage, ConversationSnapshot, ToolCallId};
use assistant_protocol::{
    AgentVariant, ApprovalMode, ChildTaskEvent, ChildTaskId, ChildTaskSnapshot, ChildTaskStatus,
    RuntimeErrorCode, RuntimeErrorInfo, RuntimeEvent,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::{
    ChildTaskRecord, ChildTaskRegistry,
    cancellation::{ActiveChildGuard, ChildTaskCancellation, ChildTimeoutGuard},
    input::child_user_message,
    settlement::{child_error, child_terminal, child_terminal_with_error, settle_accepted},
    tool::{DelegateTaskInput, DelegateTaskOutput},
};
use crate::{
    ChildTaskStart, ChildTaskWorkspaceFactory, NewStoredChildTask, RuntimeStore, StoredChildTask,
    StoredChildTaskSettlement,
    context_compaction::{
        MAX_AUTOMATIC_COMPACTIONS, compact_child_context, compaction_reason_label,
        consume_execution_budget,
    },
    id,
    mcp::McpRegistry,
    observation::ObservationCoordinator,
    permission::{
        ApprovalRegistry, PermissionCoordinator, RunAuthorizationScope, RuntimeApprovalResolver,
        RuntimeToolAuthorizer,
    },
    run::RuntimeRecorder,
    session::SessionController,
    skill::{LoadSkillTool, SessionSkillCatalog, SkillActivationLatch},
};

/// 父 Run 冻结的一组子执行资源与委派配额。
pub(crate) struct ParentDelegationController {
    session: Arc<SessionController>,
    parent_run_id: assistant_protocol::RunId,
    variant: AgentVariant,
    approval_mode: ApprovalMode,
    child_agent: Arc<Agent>,
    child_compactor: Arc<crate::context_compaction::RuntimeContextCompactor>,
    store: Arc<dyn RuntimeStore>,
    registry: Arc<ChildTaskRegistry>,
    workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    permission_coordinator: Arc<PermissionCoordinator>,
    approval_registry: Arc<ApprovalRegistry>,
    infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
    events: ObservationCoordinator,
    limits: crate::DelegationConfig,
    execution_permits: Arc<Semaphore>,
    created_tasks: Mutex<u32>,
    skill_catalog: SessionSkillCatalog,
    mcp_registry: Arc<McpRegistry>,
    disclosure_context: Option<agent_types::UserMessage>,
}

pub(crate) struct ParentDelegationResources {
    pub(crate) session: Arc<SessionController>,
    pub(crate) parent_run_id: assistant_protocol::RunId,
    pub(crate) variant: AgentVariant,
    pub(crate) approval_mode: ApprovalMode,
    pub(crate) child_agent: Arc<Agent>,
    pub(crate) child_compactor: Arc<crate::context_compaction::RuntimeContextCompactor>,
    pub(crate) store: Arc<dyn RuntimeStore>,
    pub(crate) registry: Arc<ChildTaskRegistry>,
    pub(crate) workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    pub(crate) permission_coordinator: Arc<PermissionCoordinator>,
    pub(crate) approval_registry: Arc<ApprovalRegistry>,
    pub(crate) infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
    pub(crate) events: ObservationCoordinator,
    pub(crate) limits: crate::DelegationConfig,
    pub(crate) skill_catalog: SessionSkillCatalog,
    pub(crate) mcp_registry: Arc<McpRegistry>,
    pub(crate) disclosure_context: Option<agent_types::UserMessage>,
}

fn with_disclosure_context(
    mut conversation: ConversationSnapshot,
    context: Option<&agent_types::UserMessage>,
) -> ConversationSnapshot {
    if let Some(context) = context {
        conversation
            .messages
            .push(ConversationMessage::User(context.clone()));
    }
    conversation
}

impl ParentDelegationController {
    pub(crate) fn new(resources: ParentDelegationResources) -> Self {
        let execution_permits = Arc::new(Semaphore::new(
            usize::try_from(resources.limits.max_concurrent_tasks().get())
                .expect("u32 concurrency limit fits usize"),
        ));
        Self {
            session: resources.session,
            parent_run_id: resources.parent_run_id,
            variant: resources.variant,
            approval_mode: resources.approval_mode,
            child_agent: resources.child_agent,
            child_compactor: resources.child_compactor,
            store: resources.store,
            registry: resources.registry,
            workspace_factory: resources.workspace_factory,
            permission_coordinator: resources.permission_coordinator,
            approval_registry: resources.approval_registry,
            infrastructure_policies: resources.infrastructure_policies,
            events: resources.events,
            limits: resources.limits,
            execution_permits,
            created_tasks: Mutex::new(0),
            skill_catalog: resources.skill_catalog,
            mcp_registry: resources.mcp_registry,
            disclosure_context: resources.disclosure_context,
        }
    }

    pub(crate) async fn execute(
        &self,
        input: DelegateTaskInput,
        parent_call_id: ToolCallId,
        cancellation: CancellationToken,
    ) -> Result<DelegateTaskOutput, ToolError> {
        // 配额统计“父模型发起的委派尝试”而非只统计成功创建的 child。创建失败也不返还，
        // 避免异常存储/工作区状态驱动同一 Run 无界重试并制造更多部分资源。
        self.reserve_child_task_slot()?;
        let child_task_id = self.allocate_child_task_id().map_err(internal_tool_error)?;
        let parent_tool_call_id = assistant_protocol::ToolCallId::new(parent_call_id.as_str())
            .map_err(|_| ToolError::execution("delegate_task call identity is invalid"))?;
        let mut stored = self
            .store
            .create_child_task(NewStoredChildTask {
                child_task_id: child_task_id.clone(),
                session_id: self.session.id().clone(),
                parent_run_id: self.parent_run_id.clone(),
                parent_tool_call_id,
                title: input.title().to_owned(),
                system_prompt: self.child_agent.system_prompt().clone(),
                agent_variant: self.variant,
                created_at_ms: crate::runtime::now_ms().map_err(internal_tool_error)?,
            })
            .await
            .map_err(|_| ToolError::execution("child task could not be created"))?;
        self.registry
            .upsert(stored.clone())
            .map_err(internal_tool_error)?;
        let _ = self.events.send(RuntimeEvent::ChildTaskEvent {
            session_id: stored.session_id.clone(),
            parent_run_id: stored.parent_run_id.clone(),
            child_task_id: stored.child_task_id.clone(),
            event: ChildTaskEvent::Created {
                task: Box::new(child_snapshot(&stored, String::new())),
            },
        });

        let child_cancellation = Arc::new(ChildTaskCancellation::child_of(&cancellation));
        self.registry
            .activate(&stored, child_cancellation.clone())
            .map_err(internal_tool_error)?;
        let _active = ActiveChildGuard::new(self.registry.clone(), child_task_id.clone());
        // accepted 任务可以等待 permit，但不会提前创建临时目录。父级或单独取消在排队期
        // 立即唤醒；Semaphore permit 只覆盖实际工作阶段并由 RAII 释放。
        let child_token = child_cancellation.token();
        let _permit = tokio::select! {
            biased;
            () = child_token.cancelled() => {
                self.settle_accepted_and_publish(
                    &mut stored,
                    ChildTaskStatus::Cancelled,
                    None,
                ).await?;
                return Err(child_error(
                    &stored,
                    RuntimeErrorInfo::new(RuntimeErrorCode::Cancelled, "child task was cancelled"),
                ));
            }
            permit = self.execution_permits.clone().acquire_owned() => {
                permit.map_err(|_| ToolError::execution("child task concurrency controller is unavailable"))?
            }
        };
        // timeout 从取得 permit 后开始；计时任务只发布取消，主 future 继续等待 Agent/工具
        // 完成协作式清理，不能通过 drop future 假装执行已经停止。
        let _timeout = ChildTimeoutGuard::start(
            self.limits.task_timeout(),
            cancellation,
            self.registry.clone(),
            self.session.id().clone(),
            self.parent_run_id.clone(),
            child_task_id.clone(),
        );

        // accepted 先于临时目录创建；即使 Host 无法准备目录，也会形成可恢复的 failed 终态。
        let workspace = match self.workspace_factory.create(&child_task_id).await {
            Ok(workspace) => workspace,
            Err(_) => {
                let error = RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "child task workspace could not be prepared",
                );
                self.settle_accepted_and_publish(
                    &mut stored,
                    ChildTaskStatus::Failed,
                    Some(error.clone()),
                )
                .await?;
                return Err(child_error(&stored, error));
            }
        };
        let message = match child_user_message(
            &input,
            self.variant,
            self.session.environment(),
            workspace.path(),
        ) {
            Ok(message) => message,
            Err(_) => {
                let error = RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "child task input could not be prepared",
                );
                self.settle_accepted_and_publish(
                    &mut stored,
                    ChildTaskStatus::Failed,
                    Some(error.clone()),
                )
                .await?;
                return Err(child_error(&stored, error));
            }
        };
        let started_at_ms = match crate::runtime::now_ms() {
            Ok(value) => value,
            Err(_) => {
                let error = RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "child task start time is unavailable",
                );
                self.settle_accepted_and_publish(
                    &mut stored,
                    ChildTaskStatus::Failed,
                    Some(error.clone()),
                )
                .await?;
                return Err(child_error(&stored, error));
            }
        };
        let start_operation_id = match id::generate("append") {
            Ok(value) => value,
            Err(_) => {
                let error = RuntimeErrorInfo::new(
                    RuntimeErrorCode::Internal,
                    "child task operation id is unavailable",
                );
                self.settle_accepted_and_publish(
                    &mut stored,
                    ChildTaskStatus::Failed,
                    Some(error.clone()),
                )
                .await?;
                return Err(child_error(&stored, error));
            }
        };
        if self
            .store
            .start_child_task(ChildTaskStart {
                operation_id: start_operation_id,
                child_task_id: child_task_id.clone(),
                session_id: self.session.id().clone(),
                message: message.clone(),
                started_at_ms,
            })
            .await
            .is_err()
        {
            let error = RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "child task input could not be persisted",
            );
            self.settle_accepted_and_publish(
                &mut stored,
                ChildTaskStatus::Failed,
                Some(error.clone()),
            )
            .await?;
            return Err(child_error(&stored, error));
        }
        stored.status = ChildTaskStatus::Running;
        stored.started_at_ms = Some(started_at_ms);
        stored.message_count = 1;
        self.registry
            .upsert(stored.clone())
            .map_err(internal_tool_error)?;
        let _ = self.events.send(RuntimeEvent::ChildTaskEvent {
            session_id: stored.session_id.clone(),
            parent_run_id: stored.parent_run_id.clone(),
            child_task_id: stored.child_task_id.clone(),
            event: ChildTaskEvent::Started,
        });

        let task = Arc::new(
            ChildTaskRecord::recovered(
                &stored,
                Some(ConversationSnapshot::new(vec![ConversationMessage::User(
                    message,
                )])),
            )
            .map_err(internal_tool_error)?,
        );
        let authorizer = Arc::new(
            RuntimeToolAuthorizer::new(
                RunAuthorizationScope {
                    variant: self.variant,
                    approval_mode: self.approval_mode,
                },
                self.session.permission_scopes(),
                self.permission_coordinator.clone(),
                self.infrastructure_policies.clone(),
                self.session.environment(),
                Arc::new(RuntimeApprovalResolver {
                    registry: self.approval_registry.clone(),
                    session_id: self.session.id().clone(),
                    run_id: self.parent_run_id.clone(),
                    child_task_id: Some(child_task_id.clone()),
                    variant: self.variant,
                    approval_mode: self.approval_mode,
                    workspace_id: self.session.environment().workspace_id.clone(),
                    cancellation: child_token.clone(),
                    events: self.events.clone(),
                }),
            )
            .map(|authorizer| authorizer.with_mcp_registry(self.mcp_registry.clone()))
            .and_then(|authorizer| authorizer.with_additional_private_root(workspace.path()))
            .map_err(internal_tool_error)?,
        );
        let skill_activation_latch = Arc::new(SkillActivationLatch::new(Vec::new()));
        let recorder = Arc::new(RuntimeRecorder::for_child(
            task.clone(),
            self.session.clone(),
            self.store.clone(),
            self.events.clone(),
            skill_activation_latch.clone(),
        ));
        let conversation = task
            .lock_state()
            .map_err(|_| ToolError::execution("child task journal is unavailable"))?
            .journal
            .as_ref()
            .ok_or_else(|| ToolError::execution("child task journal is unavailable"))?
            .snapshot();
        let conversation = with_disclosure_context(conversation, self.disclosure_context.as_ref());
        let mut input = ExecutionInput { conversation };
        let mut compaction_count = 0_u32;
        let child_agent = self
            .child_agent
            .try_with_tool(LoadSkillTool::new(
                self.skill_catalog.clone(),
                skill_activation_latch,
            ))
            .map_err(|_| ToolError::execution("child load_skill tool could not be created"))?;
        let mut remaining_budget = child_agent.execution_budget().clone();
        let mut next_step = std::num::NonZeroU32::MIN;
        let (outcome, forced_error) = loop {
            let execution = child_agent.start_with_budget_at_step(
                input.clone(),
                ExecutionContext {
                    cancellation: child_token.clone(),
                    recorder: recorder.clone(),
                    authorizer: authorizer.clone(),
                },
                remaining_budget.clone(),
                next_step,
            );
            let mut events = execution.events;
            let event_sender = self.events.clone();
            let event_session_id = stored.session_id.clone();
            let event_parent_run_id = stored.parent_run_id.clone();
            let event_child_task_id = stored.child_task_id.clone();
            let event_drain = tokio::spawn(async move {
                while let Some(event) = events.next().await {
                    if let Some(event) = super::events::project(event) {
                        let _ = event_sender.send(RuntimeEvent::ChildTaskEvent {
                            session_id: event_session_id.clone(),
                            parent_run_id: event_parent_run_id.clone(),
                            child_task_id: event_child_task_id.clone(),
                            event,
                        });
                    }
                }
            });
            let outcome = execution.completion.await;
            let _ = event_drain.await;
            let (consumption, compaction_reason) = match &outcome {
                agent_core::ExecutionOutcome::CompactionRequired {
                    reason,
                    consumption,
                    ..
                } => (consumption, Some(*reason)),
                agent_core::ExecutionOutcome::ContinuationRequired { consumption, .. } => {
                    (consumption, None)
                }
                _ => break (Some(outcome), None),
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
                        RuntimeErrorCode::Internal,
                        "child step sequence overflowed",
                    )),
                );
            };
            next_step = std::num::NonZeroU32::new(advanced_step)
                .expect("a non-zero step plus consumption stays non-zero");
            let Some(reason) = compaction_reason else {
                input = ExecutionInput {
                    conversation: with_disclosure_context(
                        task.lock_state()
                            .map_err(|_| ToolError::execution("child task journal is unavailable"))?
                            .journal
                            .as_ref()
                            .ok_or_else(|| {
                                ToolError::execution("child task journal is unavailable")
                            })?
                            .snapshot(),
                        self.disclosure_context.as_ref(),
                    ),
                };
                continue;
            };
            if compaction_count >= MAX_AUTOMATIC_COMPACTIONS {
                break (
                    None,
                    Some(RuntimeErrorInfo::new(
                        RuntimeErrorCode::ContextCompactionFailed,
                        format!(
                            "child context compaction recovery limit reached (reason={})",
                            compaction_reason_label(reason)
                        ),
                    )),
                );
            }
            compaction_count += 1;
            match compact_child_context(
                self.child_compactor.as_ref(),
                task.as_ref(),
                self.store.as_ref(),
                child_token.clone(),
            )
            .await
            {
                Ok((replacement, product_message_count)) => {
                    stored.body_generation = stored.body_generation.saturating_add(1);
                    stored.message_count = product_message_count;
                    self.registry
                        .upsert(stored.clone())
                        .map_err(internal_tool_error)?;
                    input = ExecutionInput {
                        conversation: with_disclosure_context(
                            replacement,
                            self.disclosure_context.as_ref(),
                        ),
                    };
                }
                Err(error) if error.is_cancelled() || child_token.is_cancelled() => {
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
                            RuntimeErrorCode::ContextCompactionFailed,
                            format!(
                                "child context compaction failed (reason={})",
                                compaction_reason_label(reason)
                            ),
                        )),
                    );
                }
            }
        };

        let terminal = match forced_error {
            Some(error) => child_terminal_with_error(error),
            None => child_terminal(
                outcome.expect("child execution without forced error has an outcome"),
                &task,
                child_cancellation.reason(),
            ),
        };
        let settlement = StoredChildTaskSettlement {
            operation_id: id::generate("append")
                .map_err(|_| ToolError::execution("child settlement id is unavailable"))?,
            child_task_id: child_task_id.clone(),
            session_id: self.session.id().clone(),
            status: terminal.status,
            cancel_requested: terminal.cancel_requested,
            error: terminal.error.clone(),
            messages: terminal.messages.clone(),
            final_message_id: terminal.final_message_id.clone(),
            finished_at_ms: crate::runtime::now_ms().map_err(internal_tool_error)?,
        };
        self.store
            .settle_child_task(settlement.clone())
            .await
            .map_err(|_| {
                ToolError::execution("child task terminal state could not be persisted")
            })?;
        task.commit_terminal(&settlement.messages)
            .map_err(internal_tool_error)?;
        stored.status = settlement.status;
        stored.cancel_requested |= settlement.cancel_requested;
        stored.error = settlement.error.clone();
        stored.final_message_id = settlement.final_message_id;
        stored.finished_at_ms = Some(settlement.finished_at_ms);
        stored.message_count += u64::try_from(settlement.messages.len()).unwrap_or(u64::MAX);
        self.registry
            .upsert(stored.clone())
            .map_err(internal_tool_error)?;
        let owner = assistant_protocol::ConversationOwner::ChildTask {
            session_id: stored.session_id.clone(),
            child_task_id: stored.child_task_id.clone(),
        };
        let _ = self.events.send(RuntimeEvent::ConversationCommitted {
            owner: owner.clone(),
            generation: stored.body_generation,
        });
        if let Some(step) = terminal.final_step {
            let _ = self.events.send(RuntimeEvent::StepCommitted {
                owner,
                step,
                generation: stored.body_generation,
            });
        }
        let _ = self.events.send(RuntimeEvent::ChildTaskEvent {
            session_id: stored.session_id.clone(),
            parent_run_id: stored.parent_run_id.clone(),
            child_task_id: stored.child_task_id.clone(),
            event: ChildTaskEvent::Finished {
                status: stored.status,
                error: stored.error.clone(),
            },
        });

        // workspace lease 到这里仍存活；只有终态已可靠提交后才允许 Drop 清理。
        drop(workspace);
        if terminal.status == ChildTaskStatus::Completed {
            Ok(DelegateTaskOutput::completed(
                &child_task_id,
                terminal.result.unwrap_or_default(),
            ))
        } else {
            let fallback_error = if terminal.status == ChildTaskStatus::Cancelled {
                RuntimeErrorInfo::new(RuntimeErrorCode::Cancelled, "child task was cancelled")
            } else {
                RuntimeErrorInfo::new(RuntimeErrorCode::Internal, "child task did not complete")
            };
            Err(child_error(
                &stored,
                terminal.error.unwrap_or(fallback_error),
            ))
        }
    }

    fn allocate_child_task_id(&self) -> Result<ChildTaskId, crate::RuntimeError> {
        for _ in 0..id::GENERATION_ATTEMPTS {
            let value =
                id::generate("ct").map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                    component: "child task id random source",
                })?;
            let task_id = ChildTaskId::new(value).map_err(|_| {
                crate::RuntimeError::InternalStateUnavailable {
                    component: "child task id generator",
                }
            })?;
            if !self.registry.contains(&task_id)? {
                return Ok(task_id);
            }
        }
        Err(crate::RuntimeError::InternalStateUnavailable {
            component: "child task id collision",
        })
    }

    async fn settle_accepted_and_publish(
        &self,
        stored: &mut StoredChildTask,
        status: ChildTaskStatus,
        error: Option<RuntimeErrorInfo>,
    ) -> Result<(), ToolError> {
        settle_accepted(
            self.store.as_ref(),
            self.registry.as_ref(),
            stored,
            status,
            error,
        )
        .await?;
        let _ = self.events.send(RuntimeEvent::ChildTaskEvent {
            session_id: stored.session_id.clone(),
            parent_run_id: stored.parent_run_id.clone(),
            child_task_id: stored.child_task_id.clone(),
            event: ChildTaskEvent::Finished {
                status: stored.status,
                error: stored.error.clone(),
            },
        });
        Ok(())
    }

    fn reserve_child_task_slot(&self) -> Result<(), ToolError> {
        let mut created = self
            .created_tasks
            .lock()
            .map_err(|_| ToolError::execution("child task controller is unavailable"))?;
        if *created >= self.limits.max_tasks_per_run().get() {
            return Err(ToolError::execution_with_details(
                "this run exceeded its child task limit",
                json!({"status": "rejected", "code": "task_limit"}),
            ));
        }
        *created += 1;
        Ok(())
    }
}

fn internal_tool_error(_error: crate::RuntimeError) -> ToolError {
    ToolError::execution("child task runtime state is unavailable")
}

fn child_snapshot(stored: &StoredChildTask, final_text: String) -> ChildTaskSnapshot {
    ChildTaskSnapshot {
        child_task_id: stored.child_task_id.clone(),
        session_id: stored.session_id.clone(),
        parent_run_id: stored.parent_run_id.clone(),
        parent_tool_call_id: stored.parent_tool_call_id.clone(),
        title: stored.title.clone(),
        status: stored.status,
        variant: stored.agent_variant,
        cancel_requested: stored.cancel_requested,
        final_text,
        error: stored.error.clone(),
        created_at_ms: stored.created_at_ms,
        started_at_ms: stored.started_at_ms,
        finished_at_ms: stored.finished_at_ms,
    }
}
