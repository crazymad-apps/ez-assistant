use super::*;

impl VolatileRuntimeStore {
    pub(super) fn stop_goal(&self, stop: GoalStop) -> StoreFuture<'_, GoalStopResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current = state
                .goals
                .get(&stop.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            let stopped = &stop.stopped_goal;
            if current.goal_id != stop.goal_id
                || current.generation != stop.expected_generation
                || current.state != StoredGoalState::Running
                || stopped.goal_id != current.goal_id
                || stopped.session_id != current.session_id
                || stopped.objective != current.objective
                || stopped.state != StoredGoalState::Paused
                || stopped.pause_reason != Some(StoredGoalPauseReason::UserStopped)
                || stopped.generation
                    != current
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal generation is exhausted"))?
                || stopped.turn != current.turn
                || stopped.budget != current.budget
                || stopped.consecutive_failures != current.consecutive_failures
                || stopped.created_at_ms != current.created_at_ms
                || stopped.updated_at_ms < current.updated_at_ms
                || stopped.completed_at_ms.is_some()
            {
                return Err(conflict("Goal stop generation is stale"));
            }
            let removed_input_ids = state
                .inputs
                .values()
                .filter(|input| {
                    input.session_id == stop.session_id
                        && input.state == StoredInputState::Queued
                        && input.origin == InputOrigin::Runtime
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            binding.goal_id == stop.goal_id
                                && binding.generation == stop.expected_generation
                        })
                })
                .map(|input| input.input_id.clone())
                .collect::<Vec<_>>();
            if removed_input_ids.len() > 1 {
                return Err(conflict("Goal has multiple queued continuations"));
            }
            let active_run_ids = state
                .runs
                .values()
                .filter(|run| !run.status.is_terminal())
                .filter_map(|run| {
                    let input = state.inputs.get(&run.input_id)?;
                    (input.state == StoredInputState::Committed
                        && input.goal_binding.as_ref().is_some_and(|binding| {
                            binding.goal_id == stop.goal_id
                                && binding.generation == stop.expected_generation
                        }))
                    .then_some(run.run_id.clone())
                })
                .collect::<Vec<_>>();
            if active_run_ids.len() > 1 {
                return Err(conflict("Goal has multiple active Runs"));
            }
            for input_id in &removed_input_ids {
                state.inputs.remove(input_id);
                state.runs.retain(|_, run| &run.input_id != input_id);
            }
            let cancelling_run_id = active_run_ids.into_iter().next();
            if let Some(run_id) = cancelling_run_id.as_ref() {
                let run = state
                    .runs
                    .get_mut(run_id)
                    .ok_or_else(|| conflict("active Goal Run disappeared"))?;
                run.cancel_requested = true;
                run.status = RunStatus::Cancelling;
            }
            state
                .goals
                .insert(stop.session_id, stop.stopped_goal.clone());
            Ok(GoalStopResult {
                goal: stop.stopped_goal,
                removed_input_ids,
                cancelling_run_id,
            })
        })
    }

    pub(super) fn clear_goal(&self, clear: GoalClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current = state
                .goals
                .get(&clear.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            if current.goal_id != clear.goal_id
                || current.generation != clear.expected_generation
                || current.state == StoredGoalState::Running
            {
                return Err(conflict("Goal cannot be cleared"));
            }
            let has_active_run = state.runs.values().any(|run| {
                !run.status.is_terminal()
                    && state.inputs.get(&run.input_id).is_some_and(|input| {
                        input
                            .goal_binding
                            .as_ref()
                            .is_some_and(|binding| binding.goal_id == clear.goal_id)
                    })
            });
            if has_active_run {
                return Err(conflict("Goal still has an active Run"));
            }
            state.goals.remove(&clear.session_id);
            Ok(())
        })
    }

    pub(super) fn resume_goal_with_held_input(
        &self,
        resume: GoalHeldInputResume,
    ) -> StoreFuture<'_, GoalHeldInputResumeResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let current_goal = state
                .goals
                .get(&resume.session_id)
                .ok_or_else(|| conflict("Goal does not exist"))?;
            let goal = &resume.resumed_goal;
            if current_goal.goal_id != resume.expected_goal_id
                || current_goal.generation != resume.expected_generation
                || current_goal.state != StoredGoalState::Paused
                || goal.goal_id != current_goal.goal_id
                || goal.session_id != current_goal.session_id
                || goal.objective != current_goal.objective
                || goal.state != StoredGoalState::Running
                || goal.pause_reason.is_some()
                || goal.generation
                    != current_goal
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal generation is exhausted"))?
                || goal.turn
                    != current_goal
                        .turn
                        .checked_add(1)
                        .ok_or_else(|| conflict("Goal turn is exhausted"))?
                || goal.budget != current_goal.budget
                || goal.consecutive_failures != current_goal.consecutive_failures
                || goal.created_at_ms != current_goal.created_at_ms
                || goal.completed_at_ms.is_some()
            {
                return Err(conflict("held Input Goal resume is stale"));
            }
            let input = state
                .inputs
                .get(&resume.input_id)
                .ok_or_else(|| conflict("held Input does not exist"))?;
            if input.session_id != resume.session_id
                || input.state != StoredInputState::Queued
                || input.origin != InputOrigin::User
                || input.goal_binding.is_some()
                || input.user_message_id != resume.message.id
            {
                return Err(conflict("Input is not held user guidance"));
            }
            let binding = super::super::GoalInputBinding {
                goal_id: goal.goal_id.clone(),
                generation: goal.generation,
                turn: goal.turn,
                reply_route: input
                    .goal_binding
                    .as_ref()
                    .and_then(|binding| binding.reply_route.clone()),
            };
            validate_input_message(InputOrigin::User, Some(&binding), None, &resume.message)
                .map_err(|_| conflict("held Goal resume message is invalid"))?;
            let run = state
                .runs
                .values()
                .find(|run| run.input_id == resume.input_id && run.status == RunStatus::Accepted)
                .cloned()
                .ok_or_else(|| conflict("held Input has no accepted Run"))?;
            let input = state
                .inputs
                .get_mut(&resume.input_id)
                .expect("checked held Input");
            input.goal_binding = Some(binding);
            input.queued_message = Some(resume.message);
            let input = input.clone();
            state
                .goals
                .insert(resume.session_id, resume.resumed_goal.clone());
            Ok(GoalHeldInputResumeResult {
                goal: resume.resumed_goal,
                input,
                run,
            })
        })
    }
}
