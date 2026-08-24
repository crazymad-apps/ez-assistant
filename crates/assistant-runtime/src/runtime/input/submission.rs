//! 普通与 Goal 用户输入共享的准备、原子接受和 Session 投影。

use assistant_protocol::{SessionTitleOrigin, SubmitInputRequest, SubmitInputResult};

use super::super::{
    AssistantRuntime,
    goal::{GoalSubmissionPersistence, PreparedGoalSubmission},
};
use crate::{
    InputOrigin, NewStoredInput, RuntimeError, RuntimeResult,
    run::{RunRecord, allocate_run_id, create_user_message},
    session::InputRecord,
};

impl AssistantRuntime {
    /// 先持久化 Input 与首次 Run，再把它加入目标 Session 的执行 lane。
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
        let snapshot = {
            let mut state = session.lock_state()?;
            let record = RunRecord::accepted(&accepted.run, Vec::new());
            let snapshot = record.snapshot();
            state.current_variant = accepted.input.agent_variant;
            if let Some(title) = generated_title {
                state.title = title;
            }
            state.runs.insert(accepted.run.run_id.clone(), record);
            let changes_user_queue = accepted.input.goal_binding.is_none();
            if changes_user_queue {
                state.user_inputs.push_back(accepted.input.input_id.clone());
                state.queue_revision = state.queue_revision.saturating_add(1);
                if state.goal.is_some() {
                    state.resume_required = true;
                }
            } else {
                state.goal_inputs.push_back(accepted.input.input_id.clone());
            }
            if let Some(PreparedGoalSubmission { control, .. }) = prepared_goal {
                state.goal = Some(control);
            }
            state.queue_paused_by_user = false;
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
        if let Some(goal) = goal_snapshot {
            self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
                session_id: session.id().clone(),
                goal_id: goal.goal_id,
                generation: goal.generation,
            });
        }
        if goal_binding.is_none() {
            let revision = session.lock_state()?.queue_revision;
            self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
                session_id: session.id().clone(),
                revision,
            });
        }
        self.wake_queue(session.clone())?;
        Ok(SubmitInputResult {
            input_id: accepted.input.input_id,
            run: snapshot,
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
