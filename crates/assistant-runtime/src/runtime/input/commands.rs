//! 输入准入、排队取消、恢复与失败 Run 重试命令。

use assistant_protocol::{
    CancelQueuedInputRequest, CancelQueuedInputResult, InputId, ResumeSessionRequest,
    ResumeSessionResult, RetryRunRequest, RetryRunResult, RunStatus, SubmitInputRequest,
    SubmitInputResult,
};

use super::AssistantRuntime;
use crate::{
    NewStoredInput, NewStoredRunAttempt, RuntimeError, RuntimeResult, StoredInputState,
    run::{RunRecord, allocate_run_id, create_user_message},
    session::InputRecord,
};

impl AssistantRuntime {
    /// 先持久化 Input 与首次 Run，再把它加入目标 Session 的执行队列。
    pub async fn submit_input(
        &self,
        request: SubmitInputRequest,
    ) -> RuntimeResult<SubmitInputResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if request.message.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "message must not be blank",
            });
        }
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        if let Some(key) = request.idempotency_key.as_ref()
            && let Some((input_id, run)) = session.find_idempotent(key)?
        {
            return Ok(SubmitInputResult { input_id, run });
        }
        session.ensure_healthy()?;
        let files = self.resolve_file_references(&request.session_id, &request.attachment_ids)?;
        let message = create_user_message(request.message, files)?;
        let (input_id, run_id) = {
            let state = session.lock_state()?;
            (self.allocate_input_id(&state)?, allocate_run_id(&state)?)
        };
        let accepted = self
            .store
            .accept_input(NewStoredInput {
                input_id: input_id.clone(),
                run_id: run_id.clone(),
                session_id: session.id().clone(),
                idempotency_key: request.idempotency_key,
                message,
                accepted_at_ms: super::super::now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("accept input", source))?;
        if accepted.is_duplicate {
            let state = session.lock_state()?;
            let run = state
                .runs
                .get(&accepted.run.run_id)
                .map(RunRecord::snapshot)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "idempotent run projection",
                })?;
            return Ok(SubmitInputResult {
                input_id: accepted.input.input_id,
                run,
            });
        }
        let snapshot = {
            let mut state = session.lock_state()?;
            let record = RunRecord::accepted(
                accepted.run.run_id.clone(),
                session.id().clone(),
                accepted.input.input_id.clone(),
                1,
                Vec::new(),
            );
            let snapshot = record.snapshot();
            state.runs.insert(accepted.run.run_id.clone(), record);
            state
                .runnable_inputs
                .push_back(accepted.input.input_id.clone());
            state.inputs.insert(
                accepted.input.input_id.clone(),
                InputRecord {
                    stored: accepted.input.clone(),
                    first_run_id: accepted.run.run_id.clone(),
                    latest_run_id: accepted.run.run_id.clone(),
                },
            );
            snapshot
        };
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: snapshot.run_id.clone(),
        });
        self.wake_queue(session.clone())?;
        Ok(SubmitInputResult {
            input_id: accepted.input.input_id,
            run: snapshot,
        })
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
        }
        self.store
            .cancel_queued_input(session.id(), &request.input_id)
            .await
            .map_err(|source| RuntimeError::from_store("cancel queued input", source))?;
        let mut state = session.lock_state()?;
        state.runnable_inputs.retain(|id| id != &request.input_id);
        if state.retry_override_input.as_ref() == Some(&request.input_id) {
            state.retry_override_input = None;
        }
        state
            .runs
            .retain(|_, run| run.input_id() != &request.input_id);
        state.inputs.remove(&request.input_id);
        if state
            .inputs
            .values()
            .all(|input| input.stored.state != StoredInputState::Queued)
        {
            state.resume_required = false;
        }
        drop(state);
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
            state.retry_override_input = None;
        }
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
        let (input_id, new_run_id) = {
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
            if state
                .inputs
                .get(source.input_id())
                .is_none_or(|input| input.latest_run_id != request.run_id)
            {
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
            (source.input_id().clone(), allocate_run_id(&state)?)
        };
        let stored = self
            .store
            .create_run_attempt(NewStoredRunAttempt {
                run_id: new_run_id.clone(),
                source_run_id: request.run_id,
                session_id: request.session_id,
                created_at_ms: super::super::now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("retry run", source))?;
        let snapshot = {
            let mut state = session.lock_state()?;
            let record = RunRecord::accepted(
                stored.run_id.clone(),
                stored.session_id.clone(),
                input_id.clone(),
                stored.attempt,
                Vec::new(),
            );
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
                .runnable_inputs
                .iter()
                .position(|id| {
                    state
                        .inputs
                        .get(id)
                        .is_some_and(|input| input.stored.queue_order > queue_order)
                })
                .unwrap_or(state.runnable_inputs.len());
            state.runnable_inputs.insert(position, input_id.clone());
            if state.resume_required {
                state.retry_override_input = Some(input_id);
            }
            snapshot
        };
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: snapshot.run_id.clone(),
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
