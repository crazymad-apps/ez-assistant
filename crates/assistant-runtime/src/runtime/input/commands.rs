//! 用户 Queue 控制、Session 恢复与失败 Run 重试命令。

use assistant_protocol::{
    CancelQueuedInputRequest, CancelQueuedInputResult, InputId, PrioritizeQueuedInputRequest,
    PrioritizeQueuedInputResult, ResumeQueuedInputRequest, ResumeQueuedInputResult,
    ResumeSessionRequest, ResumeSessionResult, RetryRunRequest, RetryRunResult, RunStatus,
};

use super::AssistantRuntime;
use crate::{
    NewStoredRunAttempt, QueuePriorityChange, RuntimeError, RuntimeResult, StoredInputState,
    run::{RunRecord, allocate_run_id},
};

impl AssistantRuntime {
    /// 把指定排队输入可靠提升为下一条，并同步当前实例的执行顺序。
    pub async fn prioritize_queued_input(
        &self,
        request: PrioritizeQueuedInputRequest,
    ) -> RuntimeResult<PrioritizeQueuedInputResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let should_move = {
            let state = session.lock_state()?;
            if state.queue_revision != request.expected_revision {
                return Err(RuntimeError::QueueConflict);
            }
            let position = state
                .session_inputs
                .iter()
                .position(|input_id| input_id == &request.input_id)
                .ok_or_else(|| RuntimeError::InputNotFound {
                    session_id: request.session_id.clone(),
                    input_id: request.input_id.clone(),
                })?;
            position > 0
        };
        if should_move {
            self.store
                .prioritize_queued_input(QueuePriorityChange {
                    session_id: request.session_id.clone(),
                    input_id: request.input_id.clone(),
                })
                .await
                .map_err(|source| RuntimeError::from_store("prioritize queued input", source))?;
            let mut state = session.lock_state()?;
            let position = state
                .session_inputs
                .iter()
                .position(|input_id| input_id == &request.input_id)
                .ok_or(RuntimeError::QueueConflict)?;
            let input_id = state.session_inputs.remove(position).ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "queued input priority",
                },
            )?;
            state.session_inputs.push_front(input_id);
            let ordered_ids = state.session_inputs.iter().cloned().collect::<Vec<_>>();
            for (queue_order, input_id) in ordered_ids.into_iter().enumerate() {
                if let Some(input) = state.inputs.get_mut(&input_id) {
                    input.stored.queue_order = u64::try_from(queue_order).map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "queued input priority",
                        }
                    })?;
                }
            }
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        let queue = super::super::product::queue_snapshot(&session)?;
        if should_move {
            self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
                session_id: request.session_id,
                revision: queue.revision,
            });
        }
        Ok(PrioritizeQueuedInputResult { queue })
    }

    /// 恢复整个队列；可选目标会在恢复前可靠置顶。
    pub async fn resume_queued_input(
        &self,
        request: ResumeQueuedInputRequest,
    ) -> RuntimeResult<ResumeQueuedInputResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let should_move = {
            let state = session.lock_state()?;
            if state.queue_revision != request.expected_revision {
                return Err(RuntimeError::QueueConflict);
            }
            request
                .input_id
                .as_ref()
                .map(|input_id| {
                    state
                        .session_inputs
                        .iter()
                        .position(|candidate| candidate == input_id)
                        .ok_or_else(|| RuntimeError::InputNotFound {
                            session_id: request.session_id.clone(),
                            input_id: input_id.clone(),
                        })
                        .map(|position| position > 0)
                })
                .transpose()?
                .unwrap_or(false)
        };
        if should_move {
            self.store
                .prioritize_queued_input(QueuePriorityChange {
                    session_id: request.session_id.clone(),
                    input_id: request.input_id.clone().expect("move requires target"),
                })
                .await
                .map_err(|source| RuntimeError::from_store("resume queued input", source))?;
        }
        {
            let mut state = session.lock_state()?;
            if should_move {
                let target = request.input_id.as_ref().expect("move requires target");
                let position = state
                    .session_inputs
                    .iter()
                    .position(|candidate| candidate == target)
                    .ok_or(RuntimeError::QueueConflict)?;
                let input_id = state.session_inputs.remove(position).ok_or(
                    RuntimeError::InternalStateUnavailable {
                        component: "queued input resume",
                    },
                )?;
                state.session_inputs.push_front(input_id);
            }
            state.resume_required = false;
            state.queue_paused_by_user = false;
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        let queue = super::super::product::queue_snapshot(&session)?;
        self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
            session_id: request.session_id,
            revision: queue.revision,
        });
        self.wake_queue(session.clone())?;
        Ok(ResumeQueuedInputResult { queue })
    }

    /// 取消尚未进入规范 Conversation 的 Input。
    pub async fn cancel_queued_input(
        &self,
        request: CancelQueuedInputRequest,
    ) -> RuntimeResult<CancelQueuedInputResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        {
            let state = session.lock_state()?;
            let input =
                state
                    .inputs
                    .get(&request.input_id)
                    .ok_or_else(|| RuntimeError::InputNotFound {
                        session_id: request.session_id.clone(),
                        input_id: request.input_id.clone(),
                    })?;
            if input.stored.state != StoredInputState::Queued {
                return Err(RuntimeError::InvalidRequest {
                    reason: "input is not queued",
                });
            }
            if input.stored.goal_binding.is_some() {
                return Err(RuntimeError::InvalidRequest {
                    reason: "Goal inputs are not part of the user queue",
                });
            }
        }
        self.store
            .cancel_queued_input(session.id(), &request.input_id)
            .await
            .map_err(|source| RuntimeError::from_store("cancel queued input", source))?;
        let mut state = session.lock_state()?;
        state.session_inputs.retain(|id| id != &request.input_id);
        state
            .runs
            .retain(|_, run| run.input_id() != &request.input_id);
        state.inputs.remove(&request.input_id);
        state
            .skill_activations
            .retain(|activation| activation.input_id.as_ref() != Some(&request.input_id));
        state.queue_revision = state.queue_revision.saturating_add(1);
        if state
            .inputs
            .values()
            .all(|input| input.stored.state != StoredInputState::Queued)
        {
            state.resume_required = false;
            state.queue_paused_by_user = false;
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        drop(state);
        let revision = session.lock_state()?.queue_revision;
        self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
            session_id: session.id().clone(),
            revision,
        });
        self.wake_queue(session.clone())?;
        Ok(CancelQueuedInputResult {
            input_id: request.input_id,
        })
    }

    /// 显式解除重启恢复形成的 Session 队列暂停。
    pub async fn resume_session(
        &self,
        request: ResumeSessionRequest,
    ) -> RuntimeResult<ResumeSessionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        {
            let mut state = session.lock_state()?;
            state.resume_required = false;
            state.queue_paused_by_user = false;
            state.queue_revision = state.queue_revision.saturating_add(1);
        }
        let revision = session.lock_state()?.queue_revision;
        self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
            session_id: session.id().clone(),
            revision,
        });
        self.wake_queue(session.clone())?;
        Ok(ResumeSessionResult {
            session: session.summary()?,
        })
    }

    /// 为最新失败或中断 Run 创建新 attempt，并复用原 Input/User Message。
    pub async fn retry_run(&self, request: RetryRunRequest) -> RuntimeResult<RetryRunResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let (input_id, new_run_id, approval_mode) = {
            let state = session.lock_state()?;
            let source =
                state
                    .runs
                    .get(&request.run_id)
                    .ok_or_else(|| RuntimeError::RunNotFound {
                        session_id: request.session_id.clone(),
                        run_id: request.run_id.clone(),
                    })?;
            if !matches!(source.status(), RunStatus::Failed | RunStatus::Interrupted) {
                return Err(RuntimeError::RunNotRetryable {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                });
            }
            let Some(input) = state.inputs.get(source.input_id()) else {
                return Err(RuntimeError::RunNotRetryable {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                });
            };
            if input.stored.goal_binding.is_some() {
                return Err(RuntimeError::GoalRunRequiresResume {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                });
            }
            if input.latest_run_id != request.run_id {
                return Err(RuntimeError::RunNotRetryable {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                });
            }
            if state.active_run.is_some() {
                return Err(RuntimeError::SessionBusy {
                    session_id: request.session_id.clone(),
                });
            }
            (
                source.input_id().clone(),
                allocate_run_id(&state)?,
                state.approval_mode,
            )
        };
        let stored = self
            .store
            .create_run_attempt(NewStoredRunAttempt {
                run_id: new_run_id.clone(),
                source_run_id: request.run_id,
                session_id: request.session_id,
                approval_mode,
                created_at_ms: super::super::now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("retry run", source))?;
        let snapshot = {
            let mut state = session.lock_state()?;
            let record = RunRecord::accepted(&stored, Vec::new());
            let snapshot = record.snapshot();
            state.runs.insert(stored.run_id.clone(), record);
            let queue_order = state
                .inputs
                .get(&input_id)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "retry input projection",
                })?
                .stored
                .queue_order;
            state
                .inputs
                .get_mut(&input_id)
                .expect("checked input")
                .latest_run_id = stored.run_id.clone();
            let position = state
                .session_inputs
                .iter()
                .position(|id| {
                    state
                        .inputs
                        .get(id)
                        .is_some_and(|input| input.stored.queue_order > queue_order)
                })
                .unwrap_or(state.session_inputs.len());
            state.session_inputs.insert(position, input_id.clone());
            state.queue_revision = state.queue_revision.saturating_add(1);
            snapshot
        };
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: snapshot.run_id.clone(),
        });
        let revision = session.lock_state()?.queue_revision;
        self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
            session_id: session.id().clone(),
            revision,
        });
        self.wake_queue(session.clone())?;
        Ok(RetryRunResult { run: snapshot })
    }

    pub(crate) fn allocate_input_id(
        &self,
        state: &crate::session::SessionState,
    ) -> RuntimeResult<InputId> {
        for _ in 0..crate::id::GENERATION_ATTEMPTS {
            let id = InputId::new(crate::id::generate("i").map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "input id random source",
                }
            })?)
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "input id generator",
            })?;
            if !state.inputs.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "input id collision",
        })
    }
}
