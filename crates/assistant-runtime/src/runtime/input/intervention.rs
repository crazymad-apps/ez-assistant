//! 介入编排。队列驱动仍独占领取、提交用户消息和启动 Run。

use assistant_protocol::{
    ConversationItem, ConversationOwner, FeedbackStatus, InputId, InterveneQueuedInputRequest,
    InterventionNotExecuted, RunId, RunStatus, RuntimeEvent, StopGoalRequest, SubmitInputRequest,
    SubmitInputResult,
};
use tokio::sync::broadcast;

use super::AssistantRuntime;
use crate::{
    InputChannelSource, RuntimeError, RuntimeResult, StoredInputState, goal::GoalState,
    session::SessionController,
};

impl AssistantRuntime {
    pub(super) async fn submit_intervention(
        &self,
        request: SubmitInputRequest,
        source: InputChannelSource,
    ) -> RuntimeResult<SubmitInputResult> {
        let key = request
            .idempotency_key
            .clone()
            .ok_or(RuntimeError::InvalidRequest {
                reason: "intervention requires an input idempotency key",
            })?;
        let session = self.prepared_session(&request.session_id).await?;
        let _intervention = session.intervention_gate.lock().await;
        let mut events = self.subscribe_events();
        let input_id = {
            let _operation = self.operation_gate.read().await;
            let _binding = self.model_binding_gate.read().await;
            let _mutation = session.mutation().await;
            self.ensure_running()?;
            session.ensure_active()?;
            if let Some(accepted) = self.find_message_acceptance(&session, &key).await? {
                return self
                    .intervention_result(&session, accepted.input_id(), None, true)
                    .await
                    .and_then(|result| result.ok_or(RuntimeError::SnapshotStale));
            }
            if session.lock_state()?.executing_command.is_some() {
                return Err(RuntimeError::QueueConflict);
            }
            session.ensure_healthy()?;
            session.ensure_not_compacting()?;
            super::submission::validate_input_body(&request)?;
            self.validate_intervention_model(&session).await?;
            let (accepted, duplicate) = self
                .accept_input_locked(&session, request, source, None, None)
                .await?;
            if duplicate {
                return self
                    .intervention_result(&session, &accepted.input_id, None, true)
                    .await
                    .and_then(|result| result.ok_or(RuntimeError::SnapshotStale));
            }
            if self
                .interrupt_for_queued_input(&session, &accepted.input_id)
                .await
                .is_err()
            {
                return Ok(queued_result(
                    accepted.input_id,
                    InterventionNotExecuted::CouldNotInterrupt,
                ));
            }
            accepted.input_id
        };
        self.wait_for_intervention(&session, &input_id, None, &mut events)
            .await
    }

    /// 介入现有队列项，沿用其可靠接受的载荷与 InputId。
    pub async fn intervene_queued_input(
        &self,
        request: InterveneQueuedInputRequest,
    ) -> RuntimeResult<SubmitInputResult> {
        let session = self.prepared_session(&request.session_id).await?;
        let _intervention = session.intervention_gate.lock().await;
        let mut events = self.subscribe_events();
        let run_id = {
            let _operation = self.operation_gate.read().await;
            let _binding = self.model_binding_gate.read().await;
            let _mutation = session.mutation().await;
            self.ensure_running()?;
            session.ensure_active()?;
            let input = self.intervention_input(&session, &request.input_id)?;
            let in_queue = session
                .lock_state()?
                .queue_item_ids
                .contains(&request.input_id);
            if !in_queue {
                if input.stored.state != StoredInputState::Committed {
                    return Err(RuntimeError::QueueConflict);
                }
                return self
                    .intervention_result(
                        &session,
                        &request.input_id,
                        input.latest_run_id.as_ref(),
                        true,
                    )
                    .await
                    .and_then(|result| result.ok_or(RuntimeError::SnapshotStale));
            }
            session.ensure_healthy()?;
            session.ensure_not_compacting()?;
            self.validate_intervention_model(&session).await?;
            if self
                .interrupt_for_queued_input(&session, &request.input_id)
                .await
                .is_err()
            {
                return Ok(queued_result(
                    request.input_id,
                    InterventionNotExecuted::CouldNotInterrupt,
                ));
            }
            input.latest_run_id
        };
        self.wait_for_intervention(&session, &request.input_id, run_id.as_ref(), &mut events)
            .await
    }

    fn intervention_input(
        &self,
        session: &SessionController,
        input_id: &InputId,
    ) -> RuntimeResult<crate::session::InputRecord> {
        session
            .lock_state()?
            .inputs
            .get(input_id)
            .cloned()
            .ok_or_else(|| RuntimeError::InputNotFound {
                session_id: session.id().clone(),
                input_id: input_id.clone(),
            })
    }

    async fn validate_intervention_model(&self, session: &SessionController) -> RuntimeResult<()> {
        self.model_factory.ensure_available()?;
        let selection = session.lock_state()?.model_selection.clone();
        super::super::model::resolve_session_model(
            &self.config_registry,
            session,
            selection.as_ref(),
            self.store.as_ref(),
        )
        .await?;
        Ok(())
    }

    /// 可靠入队后在同一 mutation gate 内优先并取消。Store 写入失败时沿现有队列保留输入。
    async fn interrupt_for_queued_input(
        &self,
        session: &std::sync::Arc<SessionController>,
        input_id: &InputId,
    ) -> RuntimeResult<()> {
        if self
            .prioritize_input_locked(session, input_id, None)
            .await?
        {
            self.publish_intervention_queue(session)?;
        }
        self.interrupt_for_prioritized_input(session).await
    }

    fn publish_intervention_queue(&self, session: &SessionController) -> RuntimeResult<()> {
        let revision = session.lock_state()?.queue_revision;
        self.publish(RuntimeEvent::QueueChanged {
            session_id: session.id().clone(),
            revision,
        });
        Ok(())
    }

    /// 已可靠置顶。调用方持有 mutation gate，问答及 Goal 的状态先提交再发布。
    /// 旧 Run 的取消令牌和队列结算保持原生命周期，唤醒后仍由驱动重新校验队首。
    async fn interrupt_for_prioritized_input(
        &self,
        session: &std::sync::Arc<SessionController>,
    ) -> RuntimeResult<()> {
        let (pending, stop_goal) = {
            let state = session.lock_state()?;
            let pending = state
                .feedback
                .values()
                .filter(|request| request.status == FeedbackStatus::Pending)
                .cloned()
                .collect::<Vec<_>>();
            let stop_goal = state
                .goal
                .as_ref()
                .filter(|goal| goal.state == GoalState::Running)
                .map(|goal| StopGoalRequest {
                    session_id: session.id().clone(),
                    goal_id: goal.id.clone(),
                    expected_generation: goal.generation,
                });
            (pending, stop_goal)
        };
        for mut request in pending {
            self.persist_cancelled_feedback(&mut request).await?;
            session
                .lock_state()?
                .feedback
                .insert(request.request_id.clone(), request.clone());
            self.publish(RuntimeEvent::FeedbackChanged { request });
            session.feedback_changed.notify_waiters();
        }
        if let Some(request) = stop_goal {
            self.stop_goal_locked(request, session).await?;
        }
        let active_run = session
            .lock_state()?
            .active_run
            .as_ref()
            .filter(|run| !run.cancellation.is_cancelled())
            .map(|run| run.run_id.clone());
        if let Some(run_id) = active_run {
            self.cancel_active_run_locked(session, &run_id).await?;
        }
        self.wake_queue(session.clone())
    }

    /// 在每次检查时短暂取得 mutation gate，跨事件等待时允许旧 Run 完成可靠结算。
    async fn wait_for_intervention(
        &self,
        session: &SessionController,
        input_id: &InputId,
        run_id: Option<&RunId>,
        events: &mut broadcast::Receiver<RuntimeEvent>,
    ) -> RuntimeResult<SubmitInputResult> {
        loop {
            {
                let _mutation = session.mutation().await;
                if let Some(result) = self
                    .intervention_result(session, input_id, run_id, false)
                    .await?
                {
                    return Ok(result);
                }
            }
            tokio::select! {
                _ = self.root_cancellation.cancelled() => return Ok(queued_result(input_id.clone(), InterventionNotExecuted::Queued)),
                observed = async {
                    loop {
                        match events.recv().await {
                            Ok(RuntimeEvent::QueueChanged { session_id, .. }
                                | RuntimeEvent::RunAccepted { session_id, .. }
                                | RuntimeEvent::RunStarted { session_id, .. }
                                | RuntimeEvent::RunFinished { session_id, .. }) if &session_id == session.id() => return Ok(()),
                            Err(broadcast::error::RecvError::Lagged(_)) => return Ok(()),
                            Err(broadcast::error::RecvError::Closed) => return Err(RuntimeError::SnapshotStale),
                            _ => {},
                        }
                    }
                } => observed?,
            }
        }
    }

    /// 在 mutation gate 下从可靠消息和 Run 快照形成回执，复用正常 Run 的失败状态。
    /// 幂等提交回读首次 Run；既有队列项使用其当前 attempt。读取不会调整队列。
    async fn intervention_result(
        &self,
        session: &SessionController,
        input_id: &InputId,
        requested_run_id: Option<&RunId>,
        duplicate: bool,
    ) -> RuntimeResult<Option<SubmitInputResult>> {
        let input = self.intervention_input(session, input_id)?;
        let (run_id, reason) = {
            let state = session.lock_state()?;
            let run_id = requested_run_id
                .cloned()
                .or(input.first_run_id)
                .filter(|run_id| {
                    input.stored.state == StoredInputState::Committed
                        && (!state.queue_item_ids.contains(input_id)
                            || duplicate && requested_run_id.is_none())
                        && (duplicate
                            || state
                                .runs
                                .get(run_id)
                                .is_none_or(|run| run.status() != RunStatus::Accepted))
                });
            let reason = if state.is_faulted {
                Some(InterventionNotExecuted::SessionFaulted)
            } else if state.active_run.is_none()
                && (state.queue_paused_by_user || state.resume_required)
            {
                Some(InterventionNotExecuted::QueuePaused)
            } else if duplicate || !state.is_queue_driver_running && state.active_run.is_none() {
                Some(InterventionNotExecuted::Queued)
            } else {
                None
            };
            (run_id, reason)
        };
        let Some(run_id) = run_id else {
            return Ok(reason.map(|reason| queued_result(input_id.clone(), reason)));
        };
        let window = self
            .store
            .load_run_conversation(session.id(), &run_id)
            .await
            .map_err(|source| RuntimeError::from_store("read intervention Run history", source))?;
        let mut history = crate::StoredConversationHistory {
            generation: window.generation,
            facts: window.history.unwrap_or_default(),
        };
        let requires_terminal_history = session
            .lock_state()?
            .runs
            .get(&run_id)
            .is_none_or(|run| run.status().is_terminal());
        if requires_terminal_history {
            let committed = self
                .store
                .load_conversation_history(ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                })
                .await
                .map_err(|source| {
                    RuntimeError::from_store("read intervention Run outcome", source)
                })?;
            if committed.generation != history.generation {
                return Err(RuntimeError::SnapshotStale);
            }
            history.facts.runs = committed
                .facts
                .runs
                .into_iter()
                .filter(|run| run.run_id == run_id)
                .collect();
            history.facts.inputs = committed
                .facts
                .inputs
                .into_iter()
                .filter(|input| &input.input_id == input_id)
                .collect();
        }
        let run = self
            .project_product_run_snapshots(session, &history, &window.conversation)?
            .into_iter()
            .find(|run| run.run_id == run_id)
            .ok_or(RuntimeError::SnapshotStale)?;
        let attachments = self.session_attachment_summaries(session.id())?;
        let projection = self.projection_context(session, &attachments).await?;
        let user_message = super::super::product::project_conversation_with_history(
            &window.conversation,
            Some(&history.facts),
            &projection,
        )?
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::User(message) if message.input_id.as_ref() == Some(input_id) => {
                Some(Box::new(message))
            }
            _ => None,
        });
        Ok(Some(SubmitInputResult {
            input_id: input_id.clone(),
            run: Some(run),
            user_message,
            generation: Some(window.generation),
            not_executed: None,
        }))
    }
}

fn queued_result(input_id: InputId, reason: InterventionNotExecuted) -> SubmitInputResult {
    SubmitInputResult {
        input_id,
        run: None,
        user_message: None,
        generation: None,
        not_executed: Some(reason),
    }
}
