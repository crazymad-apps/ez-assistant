//! Session 生命周期、模型切换、Run 列表与破坏性历史重新输入。

use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{
    ArchiveSessionRequest, ArchiveSessionResult, ListRunsRequest, ListRunsResult,
    ReenterFromUserMessageRequest, ReenterFromUserMessageResult, RestoreSessionRequest,
    RestoreSessionResult, RunSnapshot, SessionLifecycle, SetSessionApprovalModeRequest,
    SetSessionApprovalModeResult, SetSessionModelRequest, SetSessionModelResult,
    SetSessionVariantRequest, SetSessionVariantResult,
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
    ApprovalModeChange, ArchiveChange, ConversationRewrite, ModelChange, NewStoredInput,
    RuntimeError, RuntimeResult, StoredInputState, VariantChange,
    journal::InMemoryJournal,
    run::{RunRecord, allocate_run_id, create_user_message},
    session::InputRecord,
};

impl AssistantRuntime {
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
        self.store
            .set_session_archive(ArchiveChange {
                session_id: request.session_id,
                archived: true,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("archive session", source))?;
        session.lock_state()?.lifecycle = SessionLifecycle::Archived;
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
        self.store
            .set_session_archive(ArchiveChange {
                session_id: request.session_id,
                archived: false,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("restore session", source))?;
        session.lock_state()?.lifecycle = SessionLifecycle::Active;
        Ok(RestoreSessionResult {
            session: session.summary()?,
        })
    }

    /// 只切换后续 Run 的 model key；冻结 System Prompt 和历史正文保持不变。
    pub async fn set_session_model(
        &self,
        request: SetSessionModelRequest,
    ) -> RuntimeResult<SetSessionModelResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        session.ensure_idle()?;
        let snapshot = self.config_registry.snapshot()?;
        let model_key = resolve_session_model_key(&snapshot, Some(request.model_key))?;
        self.store
            .set_session_model(ModelChange {
                session_id: request.session_id,
                model_key: model_key.clone(),
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session model", source))?;
        session.lock_state()?.model_key = model_key;
        Ok(SetSessionModelResult {
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
        self.store
            .set_session_variant(VariantChange {
                session_id: request.session_id,
                variant: request.variant,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session variant", source))?;
        session.lock_state()?.current_variant = request.variant;
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
        self.store
            .set_session_approval_mode(ApprovalModeChange {
                session_id: request.session_id,
                approval_mode: request.approval_mode,
                changed_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("change session approval mode", source))?;
        session.lock_state()?.approval_mode = request.approval_mode;
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
            },
            RunAuthorizationInput {
                permission_coordinator: self.permission_coordinator.clone(),
                approval_registry: self.approval_registry.clone(),
                variant: request.variant,
                approval_mode,
                run_id: run_id.clone(),
                cancellation: self.root_cancellation.child_token(),
                events: self.event_sender.clone(),
            },
            None,
        )?;
        let changed_at_ms = now_ms()?;
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
                    approval_mode,
                    message: new_message.clone(),
                    accepted_at_ms: changed_at_ms,
                },
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("rewrite conversation history", source))?;

        let run = RunRecord::accepted(
            rewritten.run.run_id.clone(),
            session.id().clone(),
            rewritten.input.input_id.clone(),
            rewritten.run.attempt,
            rewritten.run.agent_variant,
            rewritten.run.approval_mode,
            vec![new_message.id],
        );
        let run_snapshot: RunSnapshot = run.snapshot();
        {
            let mut state = session.lock_state()?;
            state
                .runs
                .retain(|_, record| !removed_inputs.contains(record.input_id()));
            state
                .inputs
                .retain(|_, input| !removed_inputs.contains(&input.stored.input_id));
            state.runnable_inputs.clear();
            state.resume_required = false;
            state.retry_override_input = None;
            state.message_count = replacement_message_count;
            state.persisted_message_count = replacement.messages.len();
            state.journal = Some(replacement_journal);
            state.current_variant = rewritten.input.agent_variant;
            state.runs.insert(rewritten.run.run_id.clone(), run);
            state
                .runnable_inputs
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
