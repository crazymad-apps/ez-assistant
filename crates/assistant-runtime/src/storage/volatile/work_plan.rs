use super::*;

impl VolatileRuntimeStore {
    pub(super) fn load_work_plan(
        &self,
        session_id: &SessionId,
    ) -> StoreFuture<'_, Option<StoredWorkPlan>> {
        let session_id = session_id.clone();
        Box::pin(async move { Ok(self.lock()?.work_plans.get(&session_id).cloned()) })
    }

    pub(super) fn mutate_work_plan(
        &self,
        mutation: WorkPlanMutation,
    ) -> StoreFuture<'_, WorkPlanMutationResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get(&mutation.session_id)
                .ok_or_else(|| conflict("work plan session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("work plan session is archived"));
            }
            let receipt_key = (mutation.session_id.clone(), mutation.operation_id.clone());
            if let Some(plan) = state.work_plan_completion_receipts.get(&receipt_key) {
                return Ok(WorkPlanMutationResult {
                    plan: plan.clone(),
                    cleared: true,
                });
            }
            if let Some(current) = state.work_plans.get(&mutation.session_id) {
                if current.last_operation_id == mutation.operation_id {
                    return Ok(WorkPlanMutationResult {
                        plan: current.clone(),
                        cleared: false,
                    });
                }
                if current.revision != mutation.expected_revision {
                    return Err(conflict("work plan revision changed"));
                }
            } else if mutation.expected_revision != 0 {
                return Err(conflict("work plan revision changed"));
            }
            let revision = mutation
                .expected_revision
                .checked_add(1)
                .ok_or_else(|| conflict("work plan revision exhausted"))?;
            let stored = StoredWorkPlan {
                session_id: mutation.session_id.clone(),
                revision,
                objective: mutation.objective,
                items: mutation.items,
                last_operation_id: mutation.operation_id,
                updated_at_ms: mutation.updated_at_ms,
            };
            let cleared = stored.items.is_empty()
                || stored
                    .items
                    .iter()
                    .all(|item| item.status == StoredTodoItemStatus::Completed);
            if cleared {
                state.work_plans.remove(&mutation.session_id);
                state
                    .work_plan_completion_receipts
                    .insert(receipt_key, stored.clone());
            } else {
                state.work_plans.insert(mutation.session_id, stored.clone());
            }
            Ok(WorkPlanMutationResult {
                plan: stored,
                cleared,
            })
        })
    }

    pub(super) fn clear_work_plan(&self, clear: WorkPlanClear) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let session = state
                .sessions
                .get(&clear.session_id)
                .ok_or_else(|| conflict("work plan session does not exist"))?;
            if session.lifecycle != StoredSessionLifecycle::Active {
                return Err(conflict("work plan session is archived"));
            }
            match state.work_plans.get(&clear.session_id) {
                Some(current) if current.revision == clear.expected_revision => {
                    state.work_plans.remove(&clear.session_id);
                    Ok(())
                }
                None if clear.expected_revision == 0 => Ok(()),
                _ => Err(conflict("work plan revision changed")),
            }
        })
    }
}
