//! Session 生命周期、模型切换、Run 列表与破坏性历史重新输入。

use std::collections::{BTreeMap, BTreeSet};

use agent_types::{ConversationMessage, ConversationSnapshot, ToolResultPart, UserPart};
use assistant_protocol::{
    ArchiveSessionRequest, ArchiveSessionResult, DeleteConfirmationToken, DeleteSessionRequest,
    DeleteSessionResult, ForkSessionRequest, ForkSessionResult, ListRunsRequest, ListRunsResult,
    PrepareDeleteSessionRequest, PrepareDeleteSessionResult, ReasoningEffortKey as ProtocolEffort,
    ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RenameSessionRequest,
    RenameSessionResult, RestoreSessionRequest, RestoreSessionResult, RunSnapshot,
    SessionLifecycle, SessionTitleOrigin, SetMessageFeedbackRequest, SetMessageFeedbackResult,
    SetSessionApprovalModeRequest, SetSessionApprovalModeResult, SetSessionModelRequest,
    SetSessionModelResult, SetSessionPinnedRequest, SetSessionPinnedResult,
    SetSessionReasoningEffortRequest, SetSessionReasoningEffortResult, SetSessionVariantRequest,
    SetSessionVariantResult,
};

use super::{
    AssistantRuntime,
    model::{
        RunAuthorizationInput, RunCompilationResources, compile_run_agent,
        resolve_session_model_key,
    },
    now_ms,
};
use crate::{
    ApprovalModeChange, ArchiveChange, ConversationRewrite, ForkSessionEnvironmentFactoryRequest,
    ForkedAttachmentReference, MessageFeedbackChange, ModelChange, NewStoredInput,
    NewStoredSession, ReasoningEffortChange, RewriteGoalEffect, RuntimeError, RuntimeResult,
    SessionDeletion, SessionFork, SessionPinnedChange, SessionTitleChange, StoredInputState,
    VariantChange,
    journal::InMemoryJournal,
    run::{RunRecord, allocate_run_id, create_user_message},
    session::InputRecord,
};

impl AssistantRuntime {
    pub(crate) const MAX_SESSION_TITLE_CHARS: usize = 80;
    const DELETE_CONFIRMATION_TTL_MS: i64 = 60_000;

    /// 从可靠 Assistant Message 及其完整工具结果闭包创建独立 Session。
    pub async fn fork_session(
        &self,
        request: ForkSessionRequest,
    ) -> RuntimeResult<ForkSessionResult> {
        let _operation = self.operation_gate.write().await;
        self.ensure_running()?;
        let source = self.session(&request.session_id)?;
        source
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = source.mutation().await;
        let (
            source_generation,
            title,
            model_key,
            reasoning_effort,
            current_variant,
            approval_mode,
            work_plan,
            source_goal,
        ) = {
            let state = source.lock_state()?;
            (
                state.body_generation,
                state.title.clone(),
                state.model_key.clone(),
                state.reasoning_effort,
                state.current_variant,
                state.approval_mode,
                state.work_plan.as_ref().map(|plan| crate::StoredWorkPlan {
                    session_id: request.session_id.clone(),
                    revision: plan.revision,
                    objective: plan.objective.clone(),
                    items: plan
                        .items
                        .iter()
                        .map(crate::StoredWorkPlanItem::from)
                        .collect(),
                    last_operation_id: "fork-source-snapshot".to_owned(),
                    updated_at_ms: plan.updated_at_ms,
                }),
                state.goal.clone(),
            )
        };
        if source_generation != request.expected_generation {
            return Err(RuntimeError::SnapshotStale);
        }
        let current = source.conversation_snapshot()?;
        let target_index = current
            .messages
            .iter()
            .position(|message| {
                matches!(message, ConversationMessage::Assistant(assistant)
                    if assistant.id.as_str() == request.fork_point.as_str())
            })
            .ok_or(RuntimeError::InvalidRequest {
                reason: "fork point is not an assistant message in this session",
            })?;
        let mut end = target_index + 1;
        while matches!(
            current.messages.get(end),
            Some(ConversationMessage::Tool(_))
        ) {
            end += 1;
        }
        let conversation = ConversationSnapshot::new(current.messages[..end].to_vec());
        conversation
            .validate_tool_exchange_pairs()
            .map_err(|_| RuntimeError::InvalidRequest {
                reason: "fork point would split a tool exchange",
            })?;

        let session_id = {
            let sessions =
                self.sessions
                    .read()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "session registry",
                    })?;
            crate::session::allocate_session_id(&sessions)?
        };
        let prepared = self
            .session_environment_factory
            .create_fork_environment(ForkSessionEnvironmentFactoryRequest {
                session_id: &session_id,
                source_system_prompt: source.system_prompt(),
                source_environment: source.environment(),
            })
            .map_err(|source| RuntimeError::SessionEnvironmentBuildFailed { source })?;

        let attachments_by_path = self
            .attachments
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment registry",
            })?
            .values()
            .filter(|attachment| attachment.session_id == request.session_id)
            .map(|attachment| {
                (
                    attachment.agent_readable_path.clone(),
                    attachment.attachment_id.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut used_attachment_ids = BTreeSet::new();
        for message in &conversation.messages {
            let ConversationMessage::User(user) = message else {
                continue;
            };
            for part in &user.parts {
                let UserPart::FileReferences(references) = part else {
                    continue;
                };
                for file in &references.files {
                    let attachment_id = attachments_by_path.get(&file.readable_path).ok_or(
                        RuntimeError::InvalidRequest {
                            reason: "fork history references an unavailable attachment",
                        },
                    )?;
                    used_attachment_ids.insert(attachment_id.clone());
                }
            }
        }
        let mut attachments = Vec::with_capacity(used_attachment_ids.len());
        let mut allocated_attachment_ids = BTreeSet::new();
        for source_attachment_id in used_attachment_ids {
            let attachment_id = self.allocate_attachment_id()?;
            if !allocated_attachment_ids.insert(attachment_id.clone()) {
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "fork attachment id collision",
                });
            }
            attachments.push(ForkedAttachmentReference {
                source_attachment_id,
                attachment_id,
            });
        }
        let tool_images = conversation
            .messages
            .iter()
            .filter_map(|message| {
                let ConversationMessage::Tool(message) = message else {
                    return None;
                };
                Some(message.result.content.as_parts())
            })
            .flatten()
            .filter_map(|part| {
                let ToolResultPart::Image { image } = part else {
                    return None;
                };
                Some(image.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let created_at_ms = now_ms()?;
        let forked_goal = source_goal
            .filter(|goal| {
                conversation.messages.iter().any(|message| {
                    matches!(message, ConversationMessage::User(user)
                        if user.id == goal.objective.source_message_id)
                })
            })
            .map(|goal| {
                Ok(goal
                    .forked(crate::goal::allocate_goal_id()?, created_at_ms)
                    .to_stored(session_id.clone()))
            })
            .transpose()?;
        let stored = self
            .store
            .fork_session(SessionFork {
                source_session_id: request.session_id,
                source_generation,
                session: NewStoredSession {
                    session_id: session_id.clone(),
                    title: fork_title(&title),
                    title_origin: SessionTitleOrigin::Generated,
                    model_key,
                    reasoning_effort,
                    system_prompt: prepared.system_prompt,
                    environment: prepared.environment,
                    current_variant,
                    approval_mode,
                    created_at_ms,
                },
                conversation,
                attachments,
                tool_images,
                work_plan,
                goal: forked_goal,
            })
            .await
            .map_err(|source| RuntimeError::from_store("fork session", source))?;
        self.permission_coordinator
            .register_scope(crate::PermissionFileScope::Session(session_id.clone()))
            .await?;
        let work_plan = stored
            .work_plan
            .map(crate::work_plan::WorkPlan::try_from)
            .transpose()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "forked work plan",
            })?;
        let goal = stored
            .goal
            .map(crate::goal::GoalControl::try_from)
            .transpose()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "forked Goal",
            })?;
        let controller = std::sync::Arc::new(crate::session::SessionController::recovered(
            stored.session,
            Vec::new(),
            Vec::new(),
            work_plan,
            goal,
        ));
        self.sessions
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .insert(session_id.clone(), controller.clone());
        let mut attachment_registry =
            self.attachments
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "attachment registry",
                })?;
        for attachment in stored.attachments {
            attachment_registry.insert(attachment.attachment_id.clone(), attachment);
        }
        drop(attachment_registry);
        let summary = controller.summary()?;
        self.publish(assistant_protocol::RuntimeEvent::SessionCreated {
            session: summary.clone(),
        });
        self.publish(assistant_protocol::RuntimeEvent::ConversationCommitted {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            },
            generation: 1,
        });
        Ok(ForkSessionResult { session: summary })
    }

    /// 返回当前精确影响并签发仅可使用一次的短期确认 token。
    pub async fn prepare_delete_session(
        &self,
        request: PrepareDeleteSessionRequest,
    ) -> RuntimeResult<PrepareDeleteSessionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_idle()?;
        if self
            .child_tasks
            .active_count_for_session(&request.session_id)?
            != 0
        {
            return Err(RuntimeError::SessionNotIdle {
                session_id: request.session_id,
            });
        }
        let impact = self
            .store
            .inspect_session_deletion(&request.session_id)
            .await
            .map_err(|source| RuntimeError::from_store("inspect session deletion", source))?;
        let now = now_ms()?;
        let expires_at_ms = now.checked_add(Self::DELETE_CONFIRMATION_TTL_MS).ok_or(
            RuntimeError::InternalStateUnavailable {
                component: "delete confirmation expiry",
            },
        )?;
        let token = self.allocate_delete_confirmation_token(now)?;
        self.delete_confirmations
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "delete confirmation registry",
            })?
            .insert(
                token.clone(),
                super::PendingDeleteConfirmation {
                    session_id: request.session_id,
                    impact: impact.clone(),
                    expires_at_ms,
                },
            );
        Ok(PrepareDeleteSessionResult {
            session: session.summary()?,
            impact,
            confirmation_token: token,
            expires_at_ms,
        })
    }

    /// 消费确认 token，并在 Store 成功后才从内存 Registry 移除 Session。
    pub async fn delete_session(
        &self,
        request: DeleteSessionRequest,
    ) -> RuntimeResult<DeleteSessionResult> {
        let _operation = self.operation_gate.write().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_idle()?;
        if self
            .child_tasks
            .active_count_for_session(&request.session_id)?
            != 0
        {
            return Err(RuntimeError::SessionNotIdle {
                session_id: request.session_id,
            });
        }
        let now = now_ms()?;
        let confirmation = self
            .delete_confirmations
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "delete confirmation registry",
            })?
            .remove(&request.confirmation_token)
            .filter(|confirmation| {
                confirmation.session_id == request.session_id && confirmation.expires_at_ms >= now
            })
            .ok_or(RuntimeError::InvalidRequest {
                reason: "delete confirmation is invalid or expired",
            })?;
        let operation_id =
            crate::id::generate("delete").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "delete operation id random source",
            })?;
        self.store
            .delete_session(SessionDeletion {
                session_id: request.session_id.clone(),
                operation_id,
                expected_impact: confirmation.impact,
            })
            .await
            .map_err(|source| RuntimeError::from_store("delete session", source))?;
        self.sessions
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .remove(&request.session_id);
        self.attachments
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment registry",
            })?
            .retain(|_, attachment| attachment.session_id != request.session_id);
        self.child_tasks.remove_session(&request.session_id)?;
        self.publish(assistant_protocol::RuntimeEvent::SessionDeleted {
            session_id: request.session_id.clone(),
        });
        Ok(DeleteSessionResult {
            session_id: request.session_id,
        })
    }

    fn allocate_delete_confirmation_token(
        &self,
        now: i64,
    ) -> RuntimeResult<DeleteConfirmationToken> {
        let mut confirmations = self.delete_confirmations.lock().map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "delete confirmation registry",
            }
        })?;
        confirmations.retain(|_, confirmation| confirmation.expires_at_ms >= now);
        for _ in 0..crate::id::GENERATION_ATTEMPTS {
            let value = crate::id::generate("delete_confirm").map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "delete confirmation random source",
                }
            })?;
            let token = DeleteConfirmationToken::new(value).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "delete confirmation generator",
                }
            })?;
            if !confirmations.contains_key(&token) {
                return Ok(token);
            }
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "delete confirmation collision",
        })
    }

    /// 按输入接收顺序和 attempt 返回指定 Session 的全部 Run。
    pub async fn list_runs(&self, request: ListRunsRequest) -> RuntimeResult<ListRunsResult> {
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        Ok(ListRunsResult {
            runs: session.run_snapshots()?,
        })
    }

    /// 把完全空闲的活动 Session 转为只读归档状态。
    pub async fn archive_session(
        &self,
        request: ArchiveSessionRequest,
    ) -> RuntimeResult<ArchiveSessionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        session.ensure_idle()?;
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_archive(ArchiveChange {
                session_id: request.session_id.clone(),
                archived: true,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("archive session", source))?;
        {
            let mut state = session.lock_state()?;
            state.lifecycle = SessionLifecycle::Archived;
            state.archived_at_ms = Some(changed_at_ms);
        }
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(ArchiveSessionResult {
            session: session.summary()?,
        })
    }

    /// 恢复归档 Session；不会解除队列暂停或自动启动 Run。
    pub async fn restore_session(
        &self,
        request: RestoreSessionRequest,
    ) -> RuntimeResult<RestoreSessionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        if session.lock_state()?.lifecycle != SessionLifecycle::Archived {
            return Err(RuntimeError::InvalidRequest {
                reason: "session is not archived",
            });
        }
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_archive(ArchiveChange {
                session_id: request.session_id.clone(),
                archived: false,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("restore session", source))?;
        {
            let mut state = session.lock_state()?;
            state.lifecycle = SessionLifecycle::Active;
            state.archived_at_ms = None;
        }
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(RestoreSessionResult {
            session: session.summary()?,
        })
    }

    /// 修改活动 Session 标题；用户标题不会再被自动标题覆盖。
    pub async fn rename_session(
        &self,
        request: RenameSessionRequest,
    ) -> RuntimeResult<RenameSessionResult> {
        let title = request.title.trim();
        if title.is_empty() || title.chars().count() > Self::MAX_SESSION_TITLE_CHARS {
            return Err(RuntimeError::InvalidRequest {
                reason: "session title must contain 1 to 80 characters",
            });
        }
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        let changed_at_ms = now_ms()?;
        self.store
            .rename_session(SessionTitleChange {
                session_id: request.session_id.clone(),
                title: title.to_owned(),
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("rename session", source))?;
        {
            let mut state = session.lock_state()?;
            state.title = title.to_owned();
            state.title_origin = SessionTitleOrigin::User;
        }
        let summary = session.summary()?;
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(RenameSessionResult { session: summary })
    }

    /// 幂等设置活动 Session 的固定状态。
    pub async fn set_session_pinned(
        &self,
        request: SetSessionPinnedRequest,
    ) -> RuntimeResult<SetSessionPinnedResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        if session.lock_state()?.is_pinned == request.is_pinned {
            return Ok(SetSessionPinnedResult {
                session: session.summary()?,
            });
        }
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_pinned(SessionPinnedChange {
                session_id: request.session_id.clone(),
                is_pinned: request.is_pinned,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("set session pinned", source))?;
        {
            let mut state = session.lock_state()?;
            state.is_pinned = request.is_pinned;
        }
        let summary = session.summary()?;
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(SetSessionPinnedResult { session: summary })
    }

    /// 保存一条可靠 Assistant Message 的本地反馈。
    pub async fn set_message_feedback(
        &self,
        request: SetMessageFeedbackRequest,
    ) -> RuntimeResult<SetMessageFeedbackResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        let exists = session
            .conversation_snapshot()?
            .messages
            .iter()
            .any(|message| {
                matches!(message, ConversationMessage::Assistant(assistant)
                if assistant.id.as_str() == request.message_id.as_str())
            });
        if !exists {
            return Err(RuntimeError::InvalidRequest {
                reason: "assistant message does not exist in session",
            });
        }
        self.store
            .set_message_feedback(MessageFeedbackChange {
                session_id: request.session_id.clone(),
                message_id: request.message_id.clone(),
                feedback: request.feedback,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("set message feedback", source))?;
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(SetMessageFeedbackResult {
            message_id: request.message_id,
            feedback: request.feedback,
        })
    }

    /// 只切换后续 Run 的 model key；冻结 System Prompt 和历史正文保持不变。
    pub async fn set_session_model(
        &self,
        request: SetSessionModelRequest,
    ) -> RuntimeResult<SetSessionModelResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        session.ensure_idle()?;
        let snapshot = self.config_registry.snapshot()?;
        let model_key = resolve_session_model_key(&snapshot, Some(request.model_key))?;
        let current_effort = session.lock_state()?.reasoning_effort;
        let model = snapshot
            .model(&model_key)
            .ok_or_else(|| RuntimeError::ModelUnavailable {
                model_key: model_key.clone(),
            })?;
        if session.lock_state()?.goal.is_some() && !model.capabilities().tool_calls {
            return Err(RuntimeError::GoalUnsupportedByModel {
                session_id: request.session_id.clone(),
            });
        }
        let reasoning_effort = downgrade_effort(current_effort, model.capabilities());
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_model(ModelChange {
                session_id: request.session_id.clone(),
                model_key: model_key.clone(),
                reasoning_effort,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session model", source))?;
        {
            let mut state = session.lock_state()?;
            state.model_key = model_key;
            state.reasoning_effort = reasoning_effort;
        }
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(SetSessionModelResult {
            session: session.summary()?,
        })
    }

    pub async fn set_session_reasoning_effort(
        &self,
        request: SetSessionReasoningEffortRequest,
    ) -> RuntimeResult<SetSessionReasoningEffortResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        let model_key = session.model_key()?;
        let snapshot = self.config_registry.snapshot()?;
        let model = snapshot
            .model(&model_key)
            .ok_or_else(|| RuntimeError::ModelUnavailable { model_key })?;
        if request
            .effort
            .is_some_and(|effort| !supports_effort(model.capabilities(), effort))
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "reasoning effort is not supported by the current model",
            });
        }
        self.store
            .set_session_reasoning_effort(ReasoningEffortChange {
                session_id: request.session_id.clone(),
                reasoning_effort: request.effort,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| {
                RuntimeError::from_store("change session reasoning effort", source)
            })?;
        session.lock_state()?.reasoning_effort = request.effort;
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: request.session_id,
        });
        Ok(SetSessionReasoningEffortResult {
            session: session.summary()?,
        })
    }

    /// 切换 Session 当前变体；活动 Run 已冻结自己的 Input 变体，不受影响。
    pub async fn set_session_variant(
        &self,
        request: SetSessionVariantRequest,
    ) -> RuntimeResult<SetSessionVariantResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_variant(VariantChange {
                session_id: request.session_id,
                variant: request.variant,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session variant", source))?;
        {
            let mut state = session.lock_state()?;
            state.current_variant = request.variant;
        }
        let summary = session.summary()?;
        self.publish(assistant_protocol::RuntimeEvent::SessionVariantChanged {
            session: summary.clone(),
        });
        Ok(SetSessionVariantResult { session: summary })
    }

    /// 切换 Session 当前审批模式；只影响之后创建的 Run。
    pub async fn set_session_approval_mode(
        &self,
        request: SetSessionApprovalModeRequest,
    ) -> RuntimeResult<SetSessionApprovalModeResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        let changed_at_ms = now_ms()?;
        self.store
            .set_session_approval_mode(ApprovalModeChange {
                session_id: request.session_id,
                approval_mode: request.approval_mode,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session approval mode", source))?;
        {
            let mut state = session.lock_state()?;
            state.approval_mode = request.approval_mode;
        }
        let summary = session.summary()?;
        self.publish(
            assistant_protocol::RuntimeEvent::SessionApprovalModeChanged {
                session: summary.clone(),
            },
        );
        Ok(SetSessionApprovalModeResult { session: summary })
    }

    /// 销毁目标 User Message 及尾段，写入全新输入并启动新的首次 Run。
    pub async fn reenter_from_user_message(
        &self,
        request: ReenterFromUserMessageRequest,
    ) -> RuntimeResult<ReenterFromUserMessageResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if request.message.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "message must not be blank",
            });
        }
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        if let Some(key) = request.idempotency_key.as_ref()
            && let Some((input_id, run)) = session.find_idempotent(key)?
        {
            return Ok(ReenterFromUserMessageResult { input_id, run });
        }
        session.ensure_idle()?;
        let current = session.conversation_snapshot()?;
        let target_message_id =
            agent_types::MessageId::new(request.message_id.as_str()).map_err(|_| {
                RuntimeError::InvalidRequest {
                    reason: "target message id is invalid",
                }
            })?;
        let target_index = current
            .messages
            .iter()
            .position(|message| {
                matches!(message, ConversationMessage::User(user) if user.id == target_message_id)
            })
            .ok_or(RuntimeError::InvalidRequest {
                reason: "target message is not a user message in this session",
            })?;
        let current_goal = session.lock_state()?.goal.clone();
        if let Some(goal) = current_goal.as_ref() {
            let objective_index = current.messages.iter().position(|message| {
                matches!(message, ConversationMessage::User(user)
                        if user.id == goal.objective.source_message_id)
            });
            if objective_index.is_none_or(|objective_index| target_index <= objective_index) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "history re-entry at or before the Goal objective requires stopping and clearing the Goal first",
                });
            }
        }
        let files = self.resolve_file_references(&request.session_id, &request.attachment_ids)?;
        let new_message = create_user_message(request.message, files, request.variant)?;
        let mut messages = current.messages[..target_index].to_vec();
        messages.push(ConversationMessage::User(new_message.clone()));
        let replacement = ConversationSnapshot::new(messages);
        replacement
            .validate_tool_exchange_pairs()
            .map_err(|_| RuntimeError::InvalidRequest {
                reason: "replacement would split a tool exchange",
            })?;
        let replacement_message_count =
            u64::try_from(replacement.messages.len()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "conversation message count",
                }
            })?;
        let replacement_journal =
            InMemoryJournal::from_snapshot(replacement.clone()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "replacement conversation journal",
                }
            })?;
        let removed_user_ids = current.messages[target_index..]
            .iter()
            .filter_map(|message| match message {
                ConversationMessage::User(user) => Some(user.id.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let removed_inputs = {
            let state = session.lock_state()?;
            state
                .inputs
                .values()
                .filter(|input| removed_user_ids.contains(&input.stored.user_message_id))
                .map(|input| input.stored.input_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
        };
        if removed_inputs.is_empty() {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "history rewrite input relation",
            });
        }

        let (input_id, run_id, approval_mode) = {
            let state = session.lock_state()?;
            (
                self.allocate_input_id(&state)?,
                allocate_run_id(&state)?,
                state.approval_mode,
            )
        };
        // 先完整构造一次 Agent，避免配置错误在破坏旧尾段之后才暴露。
        let config = self.config_registry.snapshot()?;
        let _prepared_agent = compile_run_agent(
            session.clone(),
            &config,
            RunCompilationResources {
                model_factory: self.model_factory.as_ref(),
                context_window: self.context_window.clone(),
                run_tool_factory: self.run_tool_factory.as_ref(),
                child_task_workspace_factory: self.child_task_workspace_factory.clone(),
                child_tasks: self.child_tasks.clone(),
                store: self.store.clone(),
                recall_reference_codec: self.recall_reference_codec.clone(),
            },
            RunAuthorizationInput {
                permission_coordinator: self.permission_coordinator.clone(),
                approval_registry: self.approval_registry.clone(),
                variant: request.variant,
                approval_mode,
                run_id: run_id.clone(),
                cancellation: self.root_cancellation.child_token(),
                events: self.event_sender.clone(),
                goal_binding: None,
            },
            None,
        )?;
        let changed_at_ms = now_ms()?;
        let rewritten_goal = current_goal
            .map(|goal| {
                let expected_goal_id = goal.id.clone();
                let expected_generation = goal.generation;
                let paused = goal.paused_for_recovery(changed_at_ms).map_err(|_| {
                    RuntimeError::InternalStateUnavailable {
                        component: "history rewrite Goal transition",
                    }
                })?;
                Ok((
                    paused.clone(),
                    RewriteGoalEffect {
                        expected_goal_id,
                        expected_generation,
                        goal: paused.to_stored(session.id().clone()),
                    },
                ))
            })
            .transpose()?;
        let rewritten = self
            .store
            .rewrite_from_user(ConversationRewrite {
                session_id: request.session_id.clone(),
                target_user_message_id: target_message_id,
                conversation: replacement.clone(),
                input: NewStoredInput {
                    input_id: input_id.clone(),
                    run_id: run_id.clone(),
                    session_id: request.session_id,
                    idempotency_key: request.idempotency_key,
                    agent_variant: request.variant,
                    origin: crate::InputOrigin::User,
                    goal_binding: None,
                    approval_mode,
                    message: new_message.clone(),
                    new_goal: None,
                    resumed_goal: None,
                    generated_title: None,
                    accepted_at_ms: changed_at_ms,
                },
                goal_effect: rewritten_goal.as_ref().map(|(_, effect)| effect.clone()),
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("rewrite conversation history", source))?;

        let run = RunRecord::accepted(&rewritten.run, vec![new_message.id]);
        let run_snapshot: RunSnapshot = run.snapshot();
        {
            let mut state = session.lock_state()?;
            state
                .runs
                .retain(|_, record| !removed_inputs.contains(record.input_id()));
            state
                .inputs
                .retain(|_, input| !removed_inputs.contains(&input.stored.input_id));
            state.user_inputs.clear();
            state.goal_inputs.clear();
            state.goal = rewritten_goal.map(|(goal, _)| goal);
            state.resume_required = state.goal.is_some() || !state.user_inputs.is_empty();
            state.queue_paused_by_user = false;
            state.queue_revision = state.queue_revision.saturating_add(1);
            state.message_count = replacement_message_count;
            state.persisted_message_count = replacement.messages.len();
            state.body_generation = rewritten.body_generation;
            state.journal = Some(replacement_journal);
            state.current_variant = rewritten.input.agent_variant;
            state.runs.insert(rewritten.run.run_id.clone(), run);
            state
                .user_inputs
                .push_back(rewritten.input.input_id.clone());
            state.inputs.insert(
                rewritten.input.input_id.clone(),
                InputRecord {
                    stored: rewritten.input.clone(),
                    first_run_id: rewritten.run.run_id.clone(),
                    latest_run_id: rewritten.run.run_id,
                },
            );
            debug_assert_eq!(
                state.inputs.get(&input_id).map(|input| input.stored.state),
                Some(StoredInputState::Committed)
            );
        }
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: run_id.clone(),
        });
        drop(_mutation);
        self.wake_queue(session)?;
        Ok(ReenterFromUserMessageResult {
            input_id,
            run: run_snapshot,
        })
    }
}

fn fork_title(source: &str) -> String {
    const SUFFIX: &str = "（分支）";
    let available =
        AssistantRuntime::MAX_SESSION_TITLE_CHARS.saturating_sub(SUFFIX.chars().count());
    let mut title = source.chars().take(available).collect::<String>();
    title.push_str(SUFFIX);
    title
}

fn supports_effort(
    capabilities: &crate::ResolvedModelCapabilities,
    effort: ProtocolEffort,
) -> bool {
    capabilities.reasoning.as_ref().is_some_and(|reasoning| {
        reasoning
            .efforts
            .iter()
            .any(|candidate| protocol_effort(candidate.key) == effort)
    })
}

fn downgrade_effort(
    current: Option<ProtocolEffort>,
    capabilities: &crate::ResolvedModelCapabilities,
) -> Option<ProtocolEffort> {
    let current = current?;
    capabilities
        .reasoning
        .as_ref()?
        .efforts
        .iter()
        .map(|candidate| protocol_effort(candidate.key))
        .filter(|candidate| *candidate <= current)
        .max()
}

fn protocol_effort(value: crate::ReasoningEffortKey) -> ProtocolEffort {
    match value {
        crate::ReasoningEffortKey::Low => ProtocolEffort::Low,
        crate::ReasoningEffortKey::Medium => ProtocolEffort::Medium,
        crate::ReasoningEffortKey::High => ProtocolEffort::High,
        crate::ReasoningEffortKey::XHigh => ProtocolEffort::XHigh,
        crate::ReasoningEffortKey::Max => ProtocolEffort::Max,
    }
}

#[cfg(test)]
mod effort_tests {
    use super::*;
    use crate::{
        ReasoningEffortKey, ReasoningEffortWireValue, ResolvedReasoningCapability,
        ResolvedReasoningEffort,
    };

    fn capabilities(keys: &[ReasoningEffortKey]) -> crate::ResolvedModelCapabilities {
        crate::ResolvedModelCapabilities {
            image_input: false,
            reasoning: Some(ResolvedReasoningCapability {
                efforts: keys
                    .iter()
                    .copied()
                    .map(|key| ResolvedReasoningEffort {
                        key,
                        label: key.as_str().to_owned(),
                        wire_value: ReasoningEffortWireValue::String(key.as_str().to_owned()),
                    })
                    .collect(),
                default_effort: None,
            }),
            tool_calls: true,
            tool_image_projection: agent_model::ToolImageProjection::Unsupported,
            tool_choice: agent_model::ToolChoiceCapabilities::all(),
            streaming: true,
        }
    }

    #[test]
    fn model_switch_keeps_or_only_downgrades_explicit_effort() {
        let target = capabilities(&[
            ReasoningEffortKey::Low,
            ReasoningEffortKey::High,
            ReasoningEffortKey::Max,
        ]);
        assert_eq!(
            downgrade_effort(Some(ProtocolEffort::Max), &target),
            Some(ProtocolEffort::Max)
        );
        assert_eq!(
            downgrade_effort(Some(ProtocolEffort::XHigh), &target),
            Some(ProtocolEffort::High)
        );
        assert_eq!(
            downgrade_effort(Some(ProtocolEffort::Medium), &target),
            Some(ProtocolEffort::Low)
        );
        assert_eq!(downgrade_effort(None, &target), None);
        assert_eq!(
            downgrade_effort(
                Some(ProtocolEffort::High),
                &crate::ResolvedModelCapabilities::conservative_openai_chat_completions(),
            ),
            None
        );
    }
}
