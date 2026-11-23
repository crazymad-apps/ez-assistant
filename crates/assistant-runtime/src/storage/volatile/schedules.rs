use super::VolatileRuntimeStore;
use crate::{
    ScheduleAdmission, ScheduleAdmitted, ScheduleResult, ScheduleSettlement, ScheduleTrigger,
    StoreError, StoreErrorKind, StoredSchedule,
};
use assistant_protocol::{
    ContextPage, IdempotencyKey, ListScheduleExecutionsResult, ScheduleAction, ScheduleAgentResult,
    ScheduleExecution, ScheduleExecutionId, ScheduleExecutionReason, ScheduleExecutionStatus,
    ScheduleId, ScheduleRule, ScheduleScriptResult,
};

pub(super) struct Execution {
    pub(super) record: ScheduleExecution,
    key: Option<IdempotencyKey>,
    finalized: bool,
}

impl VolatileRuntimeStore {
    pub(super) fn next_schedule_due_inner(&self) -> Result<Option<StoredSchedule>, StoreError> {
        Ok(self
            .lock()?
            .schedules
            .values()
            .filter(|plan| !plan.deleting)
            .filter_map(|plan| Some((plan.next_due_at_ms?, plan)))
            .min_by(|a, b| (a.0, &a.1.schedule_id).cmp(&(b.0, &b.1.schedule_id)))
            .map(|(_, plan)| plan.clone()))
    }

    pub(super) fn admit_schedule_inner(
        &self,
        admission: ScheduleAdmission,
    ) -> Result<ScheduleAdmitted, StoreError> {
        let state = &mut *self.lock()?;
        let Some(plan) = state.schedules.get_mut(&admission.schedule_id) else {
            return Ok(ScheduleAdmitted::Missing);
        };
        if plan.deleting {
            return Ok(ScheduleAdmitted::Missing);
        }
        let mut prior = state
            .schedule_executions
            .values()
            .filter(|e| e.record.summary.schedule_id == admission.schedule_id);
        let (key, existing) = match &admission.trigger {
            ScheduleTrigger::Manual(key) => {
                if let Some(existing) = prior.clone().find(|e| e.key.as_ref() == Some(key)) {
                    return Ok(ScheduleAdmitted::Replayed(existing.record.summary.clone()));
                }
                (Some(key.clone()), false)
            }
            ScheduleTrigger::Due { at_ms, .. } => (
                None,
                prior
                    .clone()
                    .any(|e| e.record.summary.scheduled_for_ms == Some(*at_ms)),
            ),
        };
        let active = prior
            .find(|e| e.record.summary.status.occupies_slot())
            .map(|e| e.record.summary.clone());
        if key.is_some()
            && let Some(active) = active
        {
            return Ok(ScheduleAdmitted::Busy(active));
        }
        let settings = plan.settings.clone();
        if let ScheduleTrigger::Due {
            at_ms,
            next_at_ms,
            missed,
        } = admission.trigger
        {
            if !plan.settings.configuration.enabled || plan.next_due_at_ms != Some(at_ms) {
                return Ok(ScheduleAdmitted::Ignored);
            }
            if missed || existing {
                plan.next_due_at_ms = next_at_ms;
                return Ok(ScheduleAdmitted::Ignored);
            }
        }
        if state
            .schedule_executions
            .contains_key(&admission.execution_id)
        {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "schedule execution id already exists",
            ));
        }
        if let ScheduleTrigger::Due { next_at_ms, .. } = admission.trigger {
            plan.next_due_at_ms = next_at_ms;
            plan.once_triggered = matches!(settings.configuration.rule, ScheduleRule::Once { .. });
        }
        let execution = admission.execution(settings, active.is_some());
        let started = execution.summary.status == ScheduleExecutionStatus::Running;
        state.schedule_executions.insert(
            admission.execution_id,
            Execution {
                record: execution.clone(),
                key,
                finalized: false,
            },
        );
        Ok(if started {
            ScheduleAdmitted::Started(Box::new(execution))
        } else {
            ScheduleAdmitted::Ignored
        })
    }

    pub(super) fn settle_schedule_execution_inner(
        &self,
        settlement: ScheduleSettlement,
    ) -> Result<(), StoreError> {
        if let Some(execution) = self
            .lock()?
            .schedule_executions
            .get_mut(&settlement.execution_id)
            && execution.record.summary.status == ScheduleExecutionStatus::Running
        {
            let record = &mut execution.record;
            record.summary.status = settlement.status;
            record.summary.finished_at_ms =
                Some(settlement.finished_at_ms.max(record.summary.accepted_at_ms));
            match settlement.result {
                ScheduleResult::Script(result) => record.script_result = Some(result),
                ScheduleResult::Agent(result) => record.agent_result = Some(result),
            }
        }
        Ok(())
    }

    pub(super) fn recover_schedule_executions_inner(&self, now: i64) -> Result<(), StoreError> {
        for execution in self.lock()?.schedule_executions.values_mut() {
            let record = &mut execution.record;
            if record.summary.status == ScheduleExecutionStatus::Running {
                record.summary.status = ScheduleExecutionStatus::OutcomeUnknown;
                record.summary.finished_at_ms = Some(now.max(record.summary.accepted_at_ms));
                let reason = Some(ScheduleExecutionReason::HostInterrupted);
                match record.settings.configuration.action {
                    ScheduleAction::Script { .. } => {
                        record.script_result = Some(ScheduleScriptResult {
                            reason,
                            ..Default::default()
                        })
                    }
                    ScheduleAction::Agent { .. } => {
                        record.agent_result = Some(ScheduleAgentResult {
                            reason,
                            ..Default::default()
                        })
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn get_schedule_execution_inner(
        &self,
        id: ScheduleExecutionId,
    ) -> Result<Option<ScheduleExecution>, StoreError> {
        Ok(self
            .lock()?
            .schedule_executions
            .get(&id)
            .map(|e| e.record.clone()))
    }

    pub(super) fn list_schedule_executions_inner(
        &self,
        id: ScheduleId,
        offset: u32,
        limit: u32,
    ) -> Result<Option<ListScheduleExecutionsResult>, StoreError> {
        let state = self.lock()?;
        if !state.schedules.contains_key(&id) {
            return Ok(None);
        }
        let mut summaries: Vec<_> = state
            .schedule_executions
            .values()
            .map(|e| &e.record.summary)
            .filter(|s| s.schedule_id == id)
            .collect();
        summaries.sort_unstable_by(|a, b| {
            (b.accepted_at_ms, &b.execution_id).cmp(&(a.accepted_at_ms, &a.execution_id))
        });
        let total = u32::try_from(summaries.len()).unwrap_or(u32::MAX);
        Ok(Some(ListScheduleExecutionsResult {
            executions: summaries
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect(),
            page: ContextPage {
                limit,
                offset,
                total,
            },
        }))
    }

    pub(super) fn remove_schedule(&self, id: ScheduleId) -> Result<bool, StoreError> {
        let mut state = self.lock()?;
        if !state.schedules.get(&id).is_some_and(|plan| plan.deleting) {
            return Ok(false);
        }
        let mut sessions = Vec::new();
        for execution in state
            .schedule_executions
            .values()
            .filter(|item| item.record.summary.schedule_id == id)
        {
            if execution.record.summary.status.occupies_slot() {
                return Ok(false);
            }
            sessions.extend(execution.record.agent_session_id.clone());
        }
        for session in sessions {
            super::session::purge_session(&mut state, &session);
        }
        let removed = state.schedules.remove(&id).is_some();
        if removed {
            state
                .schedule_executions
                .retain(|_, e| e.record.summary.schedule_id != id);
        }
        Ok(removed)
    }
}

impl VolatileRuntimeStore {
    pub(super) fn next_schedule_maintenance_inner(
        &self,
        after: Option<String>,
    ) -> Result<Option<(String, crate::ScheduleMaintenance)>, StoreError> {
        let state = self.lock()?;
        let after = after.as_deref().unwrap_or("");
        let deletion = state
            .schedules
            .values()
            .filter(|plan| plan.deleting)
            .find_map(|plan| {
                let key = format!("0:{}", plan.schedule_id);
                (key.as_str() > after).then_some((
                    key,
                    crate::ScheduleMaintenance::Deletion(plan.schedule_id.clone()),
                ))
            });
        if deletion.is_some() {
            return Ok(deletion);
        }
        Ok(state
            .schedule_executions
            .values()
            .filter(|execution| {
                !execution.finalized
                    && execution.record.summary.status != ScheduleExecutionStatus::Running
                    && state
                        .schedules
                        .get(&execution.record.summary.schedule_id)
                        .is_some_and(|plan| !plan.deleting)
            })
            .find_map(|execution| {
                let key = format!("1:{}", execution.record.summary.execution_id);
                (key.as_str() > after).then_some((
                    key,
                    crate::ScheduleMaintenance::Result(
                        execution.record.summary.execution_id.clone(),
                    ),
                ))
            }))
    }

    pub(super) fn mark_schedule_deleting_inner(&self, id: ScheduleId) -> Result<bool, StoreError> {
        let mut state = self.lock()?;
        let Some(plan) = state.schedules.get_mut(&id) else {
            return Ok(false);
        };
        plan.deleting = true;
        plan.next_due_at_ms = None;
        Ok(true)
    }

    // 易失 Store 没有本地文件读取权；仍实现报告与删除的原子状态变化。
    pub(super) fn archive_schedule_result_inner(
        &self,
        _id: ScheduleExecutionId,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    pub(super) async fn commit_schedule_report_inner(
        &self,
        commit: crate::ScheduleReportCommit,
    ) -> Result<Option<crate::AcceptedInput>, StoreError> {
        let input = *commit.input;
        {
            let state = self.lock()?;
            if let Some(delivery) = state
                .schedule_executions
                .get(&commit.execution_id)
                .and_then(|record| record.record.delivery.as_ref())
            {
                return Ok(state
                    .inputs
                    .get(&input.input_id)
                    .filter(|stored| {
                        stored.session_id == delivery.session_id
                            && stored.state == crate::StoredInputState::Queued
                    })
                    .map(|stored| crate::AcceptedInput::Control {
                        input: Box::new(stored.clone()),
                        run: None,
                        is_duplicate: false,
                    }));
            }
        }
        self.accept_input(input).await.map(Some)
    }

    pub(super) fn finish_schedule_result_inner(
        &self,
        id: ScheduleExecutionId,
    ) -> Result<(), StoreError> {
        let mut state = self.lock()?;
        let Some(execution) = state.schedule_executions.get_mut(&id) else {
            return Ok(());
        };
        if execution.record.summary.status.occupies_slot() {
            return Ok(());
        }
        if execution.record.settings.report_to_controller
            && execution.record.delivery.is_none()
            && execution.record.summary.status != ScheduleExecutionStatus::Skipped
        {
            return Ok(());
        }
        let session = execution.record.agent_session_id.take();
        execution.finalized = true;
        execution.record.result_finalized = true;
        if let Some(session) = session {
            super::session::purge_session(&mut state, &session);
        }
        Ok(())
    }

    pub(super) fn resolve_schedule_file_inner(
        &self,
        _id: ScheduleExecutionId,
        _file_id: String,
    ) -> Result<Option<crate::ResolvedScheduleFile>, StoreError> {
        Ok(None)
    }
}
