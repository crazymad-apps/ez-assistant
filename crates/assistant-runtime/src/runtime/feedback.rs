//! 客户端问答命令。会话 mutation gate 串行化回答/取消，可靠提交先于唤醒和事件。
use super::AssistantRuntime;
use crate::{
    FeedbackMutation, InputChannelSource, RuntimeResult,
    feedback::{answer_message, invalid, validate_answers},
};
use assistant_protocol::{
    AnswerFeedbackRequest, CancelFeedbackRequest, FeedbackMode, FeedbackRequestSnapshot,
    FeedbackStatus, IdempotencyKey, ListFeedbackRequestsRequest, RuntimeEvent, SubmitInputMode,
    SubmitInputRequest,
};

impl AssistantRuntime {
    /// 返回该会话的问答记录；刷新和断线恢复不会重新发布通知。
    pub async fn list_feedback_requests(
        &self,
        request: ListFeedbackRequestsRequest,
    ) -> RuntimeResult<Vec<FeedbackRequestSnapshot>> {
        let session = self.session_loader.load(&request.session_id).await?;
        Ok(session.lock_state()?.feedback.values().cloned().collect())
    }

    /// 接受整组答案。相同内容重试幂等，不同内容或已取消请求明确拒绝。
    pub async fn answer_feedback(
        &self,
        request: AnswerFeedbackRequest,
    ) -> RuntimeResult<FeedbackRequestSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.prepared_session(&request.session_id).await?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let mut feedback = session
            .lock_state()?
            .feedback
            .get(&request.request_id)
            .cloned()
            .ok_or_else(|| invalid("问题已失效，请刷新会话"))?;
        validate_answers(&feedback, &request.answers)?;
        if feedback.status == FeedbackStatus::Answered
            && feedback.answers.as_ref() == Some(&request.answers)
        {
            return Ok(feedback);
        }
        if feedback.status != FeedbackStatus::Pending {
            return Err(invalid("问题已处理或失效，不能再次提交"));
        }
        if feedback.mode == FeedbackMode::EndTurn {
            if session.lock_state()?.active_run.is_some() {
                return Err(invalid("问题正在发布，请稍后重试"));
            }
            let (variant, mode) = {
                let state = session.lock_state()?;
                (
                    state.current_variant,
                    if state.goal.is_some() {
                        SubmitInputMode::ResumeGoal
                    } else {
                        SubmitInputMode::Normal
                    },
                )
            };
            let input = SubmitInputRequest {
                session_id: request.session_id.clone(),
                message: answer_message(&feedback, &request.answers),
                variant,
                mode,
                attachment_ids: vec![],
                quotes: vec![],
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: Some(
                    IdempotencyKey::new(format!("feedback:{}", request.request_id))
                        .map_err(|_| invalid("问题身份无效"))?,
                ),
            };
            // 接纳路径重新取得同一 gate 并复核问题，期间发生取消不会产生新输入。
            drop(_mutation);
            self.accept_session_input(
                input,
                InputChannelSource::desktop_text(),
                None,
                Some(request),
            )
            .await?;
            return session
                .lock_state()?
                .feedback
                .get(&feedback.request_id)
                .cloned()
                .ok_or_else(|| invalid("问题已失效"));
        }
        let active = session
            .lock_state()?
            .active_run
            .as_ref()
            .filter(|active| active.run_id == feedback.run_id)
            .map(|active| active.cancellation.clone());
        if active.as_ref().is_none_or(|token| token.is_cancelled()) {
            return Err(invalid("等待已经结束，不能提交迟到回复"));
        }
        feedback.status = FeedbackStatus::Answered;
        feedback.answers = Some(request.answers);
        self.store
            .mutate_feedback(FeedbackMutation {
                request: feedback.clone(),
                expected_status: Some(FeedbackStatus::Pending),
            })
            .await
            .map_err(|e| crate::RuntimeError::from_store("answer feedback", e))?;
        session
            .lock_state()?
            .feedback
            .insert(feedback.request_id.clone(), feedback.clone());
        self.publish(RuntimeEvent::FeedbackChanged {
            request: feedback.clone(),
        });
        session.feedback_changed.notify_waiters();
        Ok(feedback)
    }

    /// 显式取消等待；不制造用户答案，轮内同时取消活动执行。
    pub async fn cancel_feedback(
        &self,
        request: CancelFeedbackRequest,
    ) -> RuntimeResult<FeedbackRequestSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.prepared_session(&request.session_id).await?;
        let mut events = self.subscribe_events();
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let mut feedback = session
            .lock_state()?
            .feedback
            .get(&request.request_id)
            .cloned()
            .ok_or_else(|| invalid("问题已失效，请刷新会话"))?;
        if feedback.status == FeedbackStatus::Cancelled {
            return Ok(feedback);
        }
        if feedback.status != FeedbackStatus::Pending {
            return Err(invalid("问题已处理或失效"));
        }
        feedback.status = FeedbackStatus::Cancelled;
        self.store
            .mutate_feedback(FeedbackMutation {
                request: feedback.clone(),
                expected_status: Some(FeedbackStatus::Pending),
            })
            .await
            .map_err(|e| crate::RuntimeError::from_store("cancel feedback", e))?;
        {
            let mut state = session.lock_state()?;
            state
                .feedback
                .insert(feedback.request_id.clone(), feedback.clone());
            // 明确取消保持已有队列，不允许旧排队消息马上越过用户操作。
            state.queue_paused_by_user = true;
            state.queue_revision = state.queue_revision.saturating_add(1);
            if feedback.mode == FeedbackMode::Wait {
                if let Some(run) = state.runs.get_mut(&feedback.run_id) {
                    run.mark_cancelling();
                }
                if let Some(active) = state
                    .active_run
                    .as_ref()
                    .filter(|a| a.run_id == feedback.run_id)
                {
                    active.cancellation.cancel();
                }
            }
        }
        self.publish(RuntimeEvent::FeedbackChanged {
            request: feedback.clone(),
        });
        self.publish(RuntimeEvent::QueueChanged {
            session_id: request.session_id,
            revision: session.lock_state()?.queue_revision,
        });
        session.feedback_changed.notify_waiters();
        drop(_mutation);
        drop(_operation);
        // 取消并发送新指令必须先等原轮退出；等待期间不持有会话或全局门禁。
        loop {
            let stopped = {
                let state = session.lock_state()?;
                state.is_faulted
                    || state
                        .active_run
                        .as_ref()
                        .is_none_or(|active| active.run_id != feedback.run_id)
            };
            if stopped {
                break;
            }
            tokio::select! {
                _ = self.root_cancellation.cancelled() => break,
                event = events.recv() => {
                    if matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)) { break; }
                }
            }
        }
        Ok(feedback)
    }
}
