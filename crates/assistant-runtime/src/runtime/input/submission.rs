//! 普通与 Goal 用户输入共享的准备、原子接受和 Session 投影。

use assistant_protocol::{SessionTitleOrigin, SubmitInputRequest, SubmitInputResult};

use super::super::{
    AssistantRuntime,
    goal::{GoalSubmissionPersistence, PreparedGoalSubmission},
};
use crate::{
    InputOrigin, NewStoredInput, RuntimeError, RuntimeResult, SkillActivationOwner,
    SkillActivationResolveError, SkillActivationTrigger, SkillName, StoredSkillActivation, id,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    run::{RunRecord, allocate_run_id, create_user_message},
    skill::render_user_activation,
};

use super::projection::project_accepted_input;

impl AssistantRuntime {
    /// 先持久化 Input 与首次 Run，再把它加入目标 Session 的执行 lane。
    pub async fn submit_input(
        &self,
        request: SubmitInputRequest,
    ) -> RuntimeResult<SubmitInputResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
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
        session.ensure_not_compacting()?;
        session.ensure_healthy()?;
        let model_key = session.model_key()?;
        let configuration = self.config_registry.snapshot()?;
        if configuration
            .active()
            .and_then(|active| active.model(&model_key))
            .is_none()
        {
            return Err(RuntimeError::ModelUnavailable { model_key });
        }
        let selected_skill = request
            .skill_name
            .as_ref()
            .map(|name| SkillName::parse(name.clone()).map_err(|_| RuntimeError::SkillNameInvalid))
            .transpose()?;
        let goal_submission = self.goal_submission(session.as_ref(), request.mode)?;
        let generated_title = automatic_session_title(&request.message);
        let files = self.resolve_file_references(&request.session_id, &request.attachment_ids)?;
        let mut message = create_user_message(request.message, files, request.variant)?;
        let (input_id, run_id, approval_mode, should_generate_title) = {
            let state = session.lock_state()?;
            (
                self.allocate_input_id(&state)?,
                allocate_run_id(&state)?,
                state.approval_mode,
                state.title_origin == SessionTitleOrigin::Generated
                    && state.inputs.is_empty()
                    && state.message_count == 0,
            )
        };
        let accepted_at_ms = super::super::now_ms()?;
        let prepared_goal = goal_submission.prepare(&mut message, accepted_at_ms)?;
        let skill_activation = selected_skill
            .as_ref()
            .map(|name| {
                let definition =
                    session
                        .skill_catalog()
                        .user_definition(name)
                        .map_err(|error| match error {
                            SkillActivationResolveError::CatalogUnavailable => {
                                RuntimeError::SkillCatalogUnavailable {
                                    session_id: session.id().clone(),
                                }
                            }
                            SkillActivationResolveError::NotFound => RuntimeError::SkillNotFound {
                                session_id: session.id().clone(),
                            },
                            SkillActivationResolveError::NotUserInvocable => {
                                RuntimeError::SkillNotUserInvocable {
                                    session_id: session.id().clone(),
                                }
                            }
                        })?;
                InternalBoundaryCoordinator::append(
                    &mut message,
                    InternalBoundaryRequest {
                        source: InternalBoundarySource::SkillActivation,
                        retention_key: Some(format!("skill:{}", definition.name.as_str())),
                        text: render_user_activation(&session.skill_catalog().revision, definition),
                    },
                )?;
                Ok(StoredSkillActivation {
                    activation_id: id::generate("skill-activation").map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "skill activation id random source",
                        }
                    })?,
                    session_id: session.id().clone(),
                    owner: SkillActivationOwner::Session(session.id().clone()),
                    run_id: Some(run_id.clone()),
                    input_id: Some(input_id.clone()),
                    message_id: message.id.clone(),
                    name: definition.name.clone(),
                    catalog_revision: session.skill_catalog().revision.clone(),
                    definition_digest: definition.definition_digest.clone(),
                    trigger: SkillActivationTrigger::User,
                    created_at_ms: accepted_at_ms,
                })
            })
            .transpose()?;
        let goal_binding = prepared_goal.as_ref().map(|goal| goal.binding.clone());
        let new_goal = prepared_goal
            .as_ref()
            .filter(|goal| matches!(goal.persistence, GoalSubmissionPersistence::Start))
            .map(|goal| goal.control.to_stored(session.id().clone()));
        let resumed_goal = prepared_goal
            .as_ref()
            .filter(|goal| matches!(goal.persistence, GoalSubmissionPersistence::Resume))
            .map(|goal| goal.control.to_stored(session.id().clone()));
        let generated_title = should_generate_title.then_some(generated_title);
        let was_proxied = session.lock_state()?.proxy.is_some();
        let accepted = self
            .store
            .accept_input(NewStoredInput {
                input_id: input_id.clone(),
                run_id: run_id.clone(),
                session_id: session.id().clone(),
                idempotency_key: request.idempotency_key,
                agent_variant: request.variant,
                origin: InputOrigin::User,
                goal_binding: goal_binding.clone(),
                cross_session_binding: None,
                skill_activation: skill_activation.clone(),
                approval_mode,
                message,
                new_goal,
                resumed_goal,
                generated_title: generated_title.clone(),
                accepted_at_ms,
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
        let goal_snapshot = prepared_goal
            .as_ref()
            .map(|prepared| super::super::product::project_goal(&prepared.control))
            .transpose()?;
        let (projection, queue_revision) = {
            let mut state = session.lock_state()?;
            let removed_deliveries = state
                .inputs
                .values()
                .filter(|candidate| {
                    candidate.stored.state == crate::StoredInputState::Queued
                        && matches!(
                            candidate.stored.cross_session_binding,
                            Some(crate::CrossSessionInputBinding::ControllerDelivery { .. })
                        )
                })
                .map(|candidate| candidate.stored.input_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            if was_proxied {
                state.proxy = None;
            }
            if !removed_deliveries.is_empty() {
                state
                    .session_inputs
                    .retain(|input_id| !removed_deliveries.contains(input_id));
                state
                    .inputs
                    .retain(|input_id, _| !removed_deliveries.contains(input_id));
                state
                    .runs
                    .retain(|_, run| !removed_deliveries.contains(run.input_id()));
            }
            if let Some(title) = generated_title {
                state.title = title;
            }
            let changes_session_queue = accepted.input.goal_binding.is_none();
            if let Some(PreparedGoalSubmission { control, .. }) = prepared_goal {
                state.goal = Some(control);
            }
            state.queue_paused_by_user = false;
            let projection = project_accepted_input(&mut state, accepted);
            if !changes_session_queue && !removed_deliveries.is_empty() {
                state.queue_revision = state.queue_revision.saturating_add(1);
            }
            let queue_revision = (changes_session_queue || !removed_deliveries.is_empty())
                .then_some(state.queue_revision);
            (projection, queue_revision)
        };
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: projection.run.run_id.clone(),
        });
        if let Some(goal) = goal_snapshot {
            self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
                session_id: session.id().clone(),
                goal_id: goal.goal_id,
                generation: goal.generation,
            });
        }
        if was_proxied {
            self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
                session_id: session.id().clone(),
            });
        }
        if let Some(revision) = queue_revision {
            self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
                session_id: session.id().clone(),
                revision,
            });
        }
        self.wake_queue(session.clone())?;
        Ok(SubmitInputResult {
            input_id: projection.input_id,
            run: projection.run,
        })
    }
}

fn automatic_session_title(message: &str) -> String {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("New Session");
    line.chars()
        .take(AssistantRuntime::MAX_SESSION_TITLE_CHARS)
        .collect()
}
