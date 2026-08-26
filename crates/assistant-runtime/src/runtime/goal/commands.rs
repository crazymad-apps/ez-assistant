//! Goal 提交意图的准入与首次领域事实构造。

use agent_types::UserMessage;
use assistant_protocol::{
    ClearGoalRequest, ClearGoalResult, ResumeGoalRequest, ResumeGoalResult, StopGoalRequest,
    StopGoalResult, SubmitInputMode,
};

use super::super::AssistantRuntime;
use crate::{
    GoalClear, GoalHeldInputResume, GoalInputBinding, GoalStop, InputOrigin, NewStoredInput,
    RuntimeError, RuntimeResult, StoreErrorKind,
    goal::{
        GoalControl, GoalState, allocate_goal_id, create_continuation_message,
        inject_resume_context, inject_start_context,
    },
    run::{RunRecord, allocate_run_id},
    session::SessionController,
};

use super::super::input::projection::project_accepted_input;

/// 通用 Input 准入完成后需要附加的 Goal 行为。
pub(in crate::runtime) enum GoalSubmission {
    None,
    Start,
    Resume(GoalControl),
}

#[derive(Clone, Copy)]
pub(in crate::runtime) enum GoalSubmissionPersistence {
    Start,
    Resume,
}

/// 首次 Goal Input 原子接受所需的领域事实。
pub(in crate::runtime) struct PreparedGoalSubmission {
    pub(in crate::runtime) control: GoalControl,
    pub(in crate::runtime) binding: GoalInputBinding,
    pub(in crate::runtime) persistence: GoalSubmissionPersistence,
}

impl AssistantRuntime {
    /// 在解析附件和构造 UserMessage 前校验 Goal 提交意图。
    pub(in crate::runtime) fn goal_submission(
        &self,
        session: &SessionController,
        mode: SubmitInputMode,
    ) -> RuntimeResult<GoalSubmission> {
        match mode {
            SubmitInputMode::Normal => Ok(GoalSubmission::None),
            SubmitInputMode::ResumeGoal => {
                let (model_key, goal) = {
                    let state = session.lock_state()?;
                    let goal = state.goal.as_ref().ok_or(RuntimeError::InvalidRequest {
                        reason: "session has no Goal to resume",
                    })?;
                    if !matches!(goal.state, GoalState::Paused(_)) {
                        return Err(RuntimeError::InvalidRequest {
                            reason: "only a paused Goal can be resumed",
                        });
                    }
                    (state.model_key.clone(), goal.clone())
                };
                self.ensure_goal_model_supported(session, &model_key)?;
                Ok(GoalSubmission::Resume(goal))
            }
            SubmitInputMode::StartGoal => {
                let model_key = {
                    let state = session.lock_state()?;
                    if state.goal.is_some() {
                        return Err(RuntimeError::GoalAlreadyExists {
                            session_id: session.id().clone(),
                        });
                    }
                    state.model_key.clone()
                };
                self.ensure_goal_model_supported(session, &model_key)?;
                Ok(GoalSubmission::Start)
            }
        }
    }

    fn ensure_goal_model_supported(
        &self,
        session: &SessionController,
        model_key: &assistant_protocol::ModelKey,
    ) -> RuntimeResult<()> {
        let snapshot = self.config_registry.snapshot()?;
        let model = snapshot
            .model(model_key)
            .ok_or_else(|| RuntimeError::ModelUnavailable {
                model_key: model_key.clone(),
            })?;
        if !model.capabilities().tool_calls {
            return Err(RuntimeError::GoalUnsupportedByModel {
                session_id: session.id().clone(),
            });
        }
        Ok(())
    }

    /// 原子暂停整个 Goal；Store 成功记录世代与取消意图后才触发活动 Run 取消令牌。
    pub async fn stop_goal(&self, request: StopGoalRequest) -> RuntimeResult<StopGoalResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let stopped_at_ms = super::super::now_ms()?;
        let (current, stopped) = {
            let state = session.lock_state()?;
            let current = state
                .goal
                .as_ref()
                .ok_or_else(|| RuntimeError::GoalNotFound {
                    session_id: request.session_id.clone(),
                })?;
            ensure_expected_goal(
                current,
                &request.goal_id,
                request.expected_generation,
                &request.session_id,
            )?;
            let stopped =
                current
                    .stop(stopped_at_ms)
                    .map_err(|_| RuntimeError::InvalidRequest {
                        reason: "only a running Goal can be stopped",
                    })?;
            (current.clone(), stopped)
        };
        let result = self
            .store
            .stop_goal(GoalStop {
                session_id: request.session_id.clone(),
                goal_id: current.id.clone(),
                expected_generation: current.generation,
                stopped_goal: stopped.to_stored(request.session_id.clone()),
            })
            .await
            .map_err(|source| map_goal_store_error(&request, source, "stop Goal"))?;
        let authoritative = GoalControl::try_from(result.goal).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "stopped Goal projection",
            }
        })?;
        let goal_snapshot = super::super::product::project_goal(&authoritative)?;
        let (cancellation, run) = {
            let mut state = session.lock_state()?;
            for input_id in &result.removed_input_ids {
                state.goal_inputs.retain(|candidate| candidate != input_id);
                state
                    .skill_activations
                    .retain(|activation| activation.input_id.as_ref() != Some(input_id));
                if let Some(input) = state.inputs.remove(input_id) {
                    state
                        .runs
                        .retain(|_, run| run.input_id() != &input.stored.input_id);
                }
            }
            let mut run_snapshot = None;
            let cancellation = result.cancelling_run_id.as_ref().and_then(|run_id| {
                if let Some(run) = state.runs.get_mut(run_id) {
                    run.mark_cancelling();
                    run_snapshot = Some(run.snapshot());
                }
                state
                    .active_run
                    .as_ref()
                    .filter(|active| &active.run_id == run_id)
                    .map(|active| active.cancellation.clone())
            });
            state.goal = Some(authoritative);
            state.resume_required = !state.session_inputs.is_empty();
            (cancellation, run_snapshot)
        };
        if result.cancelling_run_id.is_some() && cancellation.is_none() {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "active Goal Run cancellation",
            });
        }
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
        self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
            session_id: request.session_id.clone(),
            goal_id: goal_snapshot.goal_id.clone(),
            generation: goal_snapshot.generation,
        });
        if let Some(run) = &run {
            self.publish(assistant_protocol::RuntimeEvent::RunCancelling {
                session_id: request.session_id,
                run_id: run.run_id.clone(),
            });
        }
        Ok(StopGoalResult {
            goal: goal_snapshot,
            run,
        })
    }

    /// 删除非运行中的 Goal 控制器；历史正文、WorkPlan 和 held 用户输入保持不变。
    pub async fn clear_goal(&self, request: ClearGoalRequest) -> RuntimeResult<ClearGoalResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let goal = {
            let state = session.lock_state()?;
            let goal = state
                .goal
                .as_ref()
                .ok_or_else(|| RuntimeError::GoalNotFound {
                    session_id: request.session_id.clone(),
                })?;
            ensure_expected_goal(
                goal,
                &request.goal_id,
                request.expected_generation,
                &request.session_id,
            )?;
            if matches!(goal.state, GoalState::Running) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "running Goal must be stopped before it can be cleared",
                });
            }
            if state.active_run.as_ref().is_some_and(|active| {
                state
                    .runs
                    .get(&active.run_id)
                    .and_then(|run| state.inputs.get(run.input_id()))
                    .and_then(|input| input.stored.goal_binding.as_ref())
                    .is_some_and(|binding| binding.goal_id == goal.id)
            }) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "Goal still has an active Run",
                });
            }
            goal.clone()
        };
        self.store
            .clear_goal(GoalClear {
                session_id: request.session_id.clone(),
                goal_id: goal.id.clone(),
                expected_generation: goal.generation,
            })
            .await
            .map_err(|source| map_goal_store_error(&request, source, "clear Goal"))?;
        let mut state = session.lock_state()?;
        state.goal = None;
        state.goal_inputs.clear();
        state.resume_required = !state.session_inputs.is_empty();
        drop(state);
        self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
            session_id: request.session_id,
            goal_id: goal.id,
            generation: goal.generation,
        });
        Ok(ClearGoalResult { goal: None })
    }

    /// 显式恢复 Goal；可复用一条 held 用户输入，缺失时创建隐藏 Runtime continuation。
    pub async fn resume_goal(&self, request: ResumeGoalRequest) -> RuntimeResult<ResumeGoalResult> {
        if let Some(input_id) = request.input_id.clone() {
            return self.resume_goal_with_held_input(request, input_id).await;
        }
        self.resume_goal_without_input(request).await
    }

    /// 不携带新用户正文地显式恢复 Goal，并原子创建一条隐藏 Runtime continuation。
    async fn resume_goal_without_input(
        &self,
        request: ResumeGoalRequest,
    ) -> RuntimeResult<ResumeGoalResult> {
        // 与同一 Session 的 Stop/Clear/Input mutation 串行，避免校验通过后 Goal 世代被并发改写。
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;

        // 一次锁内冻结恢复所依赖的全部事实并分配 ID；后续 Store I/O 期间不持有 Session 锁。
        let (goal, model_key, input_id, run_id, variant, approval_mode) = {
            let state = session.lock_state()?;
            let goal = state
                .goal
                .as_ref()
                .ok_or_else(|| RuntimeError::GoalNotFound {
                    session_id: request.session_id.clone(),
                })?;
            ensure_expected_goal(
                goal,
                &request.goal_id,
                request.expected_generation,
                &request.session_id,
            )?;
            if !matches!(goal.state, GoalState::Paused(_)) {
                return Err(RuntimeError::GoalNotResumable {
                    session_id: request.session_id.clone(),
                    goal_id: goal.id.clone(),
                });
            }
            (
                goal.clone(),
                state.model_key.clone(),
                self.allocate_input_id(&state)?,
                allocate_run_id(&state)?,
                state.current_variant,
                state.approval_mode,
            )
        };
        self.ensure_goal_model_supported(session.as_ref(), &model_key)?;

        // 先构造下一世代及其隐藏 continuation，但此时不能提前修改内存中的权威 Goal 投影。
        let accepted_at_ms = super::super::now_ms()?;
        let resumed =
            goal.resume(accepted_at_ms)
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "Goal resume transition",
                })?;
        let message = create_continuation_message(&resumed, "explicit_resume")?;

        // accept_input 在 Store 中原子恢复 Goal，并创建绑定新世代的 Input/Run；失败不会留下半恢复状态。
        let accepted = self
            .store
            .accept_input(NewStoredInput {
                input_id,
                run_id,
                session_id: request.session_id.clone(),
                idempotency_key: None,
                agent_variant: variant,
                origin: InputOrigin::Runtime,
                goal_binding: Some(GoalInputBinding {
                    goal_id: resumed.id.clone(),
                    generation: resumed.generation,
                    turn: resumed.turn,
                }),
                cross_session_binding: None,
                skill_activation: None,
                approval_mode,
                message,
                new_goal: None,
                resumed_goal: Some(resumed.to_stored(request.session_id.clone())),
                generated_title: None,
                accepted_at_ms,
            })
            .await
            .map_err(|source| map_goal_store_error(&request, source, "resume Goal"))?;

        // 只有持久化成功后才一次性投影 Goal、队列、Input 和 Run，Store 结果保持为权威事实。
        let goal = super::super::product::project_goal(&resumed)?;
        let projection = {
            let mut state = session.lock_state()?;
            state.goal = Some(resumed);
            project_accepted_input(&mut state, accepted)
        };

        // 事件发布和队列唤醒必须晚于内存投影，使观察者看到 RunAccepted 时已能读取完整状态。
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: request.session_id.clone(),
            run_id: projection.run.run_id.clone(),
        });
        self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
            session_id: request.session_id,
            goal_id: goal.goal_id.clone(),
            generation: goal.generation,
        });
        self.wake_queue(session.clone())?;
        Ok(ResumeGoalResult {
            goal,
            run: projection.run,
        })
    }

    /// 选择一条 held 用户指导作为恢复输入；不复制正文，也不创建新的 Input/Run。
    async fn resume_goal_with_held_input(
        &self,
        request: ResumeGoalRequest,
        input_id: assistant_protocol::InputId,
    ) -> RuntimeResult<ResumeGoalResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        let (goal, model_key, mut message) = {
            let state = session.lock_state()?;
            let goal = state
                .goal
                .as_ref()
                .ok_or_else(|| RuntimeError::GoalNotFound {
                    session_id: request.session_id.clone(),
                })?;
            ensure_expected_goal(
                goal,
                &request.goal_id,
                request.expected_generation,
                &request.session_id,
            )?;
            if !matches!(goal.state, GoalState::Paused(_)) {
                return Err(RuntimeError::GoalNotResumable {
                    session_id: request.session_id.clone(),
                    goal_id: goal.id.clone(),
                });
            }
            let input = state
                .inputs
                .get(&input_id)
                .ok_or_else(|| RuntimeError::InputNotFound {
                    session_id: request.session_id.clone(),
                    input_id: input_id.clone(),
                })?;
            if !state.session_inputs.contains(&input_id)
                || input.stored.state != crate::StoredInputState::Queued
                || input.stored.origin != InputOrigin::User
                || input.stored.goal_binding.is_some()
            {
                return Err(RuntimeError::InvalidRequest {
                    reason: "input is not held user guidance",
                });
            }
            let message = input.stored.queued_message.clone().ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "held input message",
                },
            )?;
            (goal.clone(), state.model_key.clone(), message)
        };
        self.ensure_goal_model_supported(session.as_ref(), &model_key)?;
        let accepted_at_ms = super::super::now_ms()?;
        let resumed =
            goal.resume(accepted_at_ms)
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "Goal resume transition",
                })?;
        inject_resume_context(&mut message, &resumed)?;
        let result = self
            .store
            .resume_goal_with_held_input(GoalHeldInputResume {
                session_id: request.session_id.clone(),
                input_id: input_id.clone(),
                expected_goal_id: goal.id,
                expected_generation: goal.generation,
                resumed_goal: resumed.to_stored(request.session_id.clone()),
                message,
            })
            .await
            .map_err(|source| {
                map_goal_store_error(&request, source, "resume Goal with held input")
            })?;
        let authoritative = GoalControl::try_from(result.goal).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "resumed Goal projection",
            }
        })?;
        let goal = super::super::product::project_goal(&authoritative)?;
        let (snapshot, revision) = {
            let mut state = session.lock_state()?;
            let position = state
                .session_inputs
                .iter()
                .position(|candidate| candidate == &input_id)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "held input queue position",
                })?;
            state.session_inputs.remove(position);
            state.goal_inputs.push_back(input_id.clone());
            state.queue_revision = state.queue_revision.saturating_add(1);
            state.resume_required = !state.session_inputs.is_empty();
            state.goal = Some(authoritative);
            state
                .inputs
                .get_mut(&input_id)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "held input projection",
                })?
                .stored = result.input;
            let snapshot = state
                .runs
                .get(&result.run.run_id)
                .map(RunRecord::snapshot)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "held input Run projection",
                })?;
            (snapshot, state.queue_revision)
        };
        self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
            session_id: request.session_id.clone(),
            revision,
        });
        self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
            session_id: request.session_id,
            goal_id: goal.goal_id.clone(),
            generation: goal.generation,
        });
        self.wake_queue(session.clone())?;
        Ok(ResumeGoalResult {
            goal,
            run: snapshot,
        })
    }
}

fn ensure_expected_goal(
    goal: &GoalControl,
    expected_id: &assistant_protocol::GoalId,
    expected_generation: u64,
    session_id: &assistant_protocol::SessionId,
) -> RuntimeResult<()> {
    if &goal.id == expected_id && goal.generation == expected_generation {
        Ok(())
    } else {
        Err(RuntimeError::GoalGenerationConflict {
            session_id: session_id.clone(),
            goal_id: expected_id.clone(),
        })
    }
}

trait GoalCommandExpectation {
    fn session_id(&self) -> &assistant_protocol::SessionId;
    fn goal_id(&self) -> &assistant_protocol::GoalId;
}

impl GoalCommandExpectation for StopGoalRequest {
    fn session_id(&self) -> &assistant_protocol::SessionId {
        &self.session_id
    }

    fn goal_id(&self) -> &assistant_protocol::GoalId {
        &self.goal_id
    }
}

impl GoalCommandExpectation for ClearGoalRequest {
    fn session_id(&self) -> &assistant_protocol::SessionId {
        &self.session_id
    }

    fn goal_id(&self) -> &assistant_protocol::GoalId {
        &self.goal_id
    }
}

impl GoalCommandExpectation for ResumeGoalRequest {
    fn session_id(&self) -> &assistant_protocol::SessionId {
        &self.session_id
    }

    fn goal_id(&self) -> &assistant_protocol::GoalId {
        &self.goal_id
    }
}

fn map_goal_store_error(
    request: &impl GoalCommandExpectation,
    source: crate::StoreError,
    operation: &'static str,
) -> RuntimeError {
    if source.kind() == StoreErrorKind::Conflict {
        RuntimeError::GoalGenerationConflict {
            session_id: request.session_id().clone(),
            goal_id: request.goal_id().clone(),
        }
    } else {
        RuntimeError::from_store(operation, source)
    }
}

impl GoalSubmission {
    /// 在完整可见 UserMessage 上附加 Goal 上下文，并冻结首次 Goal 事实。
    pub(in crate::runtime) fn prepare(
        self,
        message: &mut UserMessage,
        accepted_at_ms: i64,
    ) -> RuntimeResult<Option<PreparedGoalSubmission>> {
        match self {
            Self::None => Ok(None),
            Self::Start => {
                let goal_id = allocate_goal_id()?;
                inject_start_context(message, &goal_id)?;
                let control =
                    GoalControl::start(goal_id, message, accepted_at_ms).map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "Goal objective snapshot",
                        }
                    })?;
                let binding = GoalInputBinding {
                    goal_id: control.id.clone(),
                    generation: control.generation,
                    turn: control.turn,
                };
                Ok(Some(PreparedGoalSubmission {
                    control,
                    binding,
                    persistence: GoalSubmissionPersistence::Start,
                }))
            }
            Self::Resume(goal) => {
                let control = goal.resume(accepted_at_ms).map_err(|_| {
                    RuntimeError::InternalStateUnavailable {
                        component: "Goal resume transition",
                    }
                })?;
                inject_resume_context(message, &control)?;
                let binding = GoalInputBinding {
                    goal_id: control.id.clone(),
                    generation: control.generation,
                    turn: control.turn,
                };
                Ok(Some(PreparedGoalSubmission {
                    control,
                    binding,
                    persistence: GoalSubmissionPersistence::Resume,
                }))
            }
        }
    }
}
