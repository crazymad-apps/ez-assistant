//! Store 启动投影到 Runtime Session registry 的唯一转换。

use std::{collections::BTreeMap, sync::Arc};

use assistant_protocol::{AttachmentId, ChildTaskId, InputId, RunId, SessionId, WorkspaceId};

use crate::{
    RecoveredRuntime, RuntimeError, RuntimeResult, StoredAttachment, StoredChildTask,
    StoredWorkspace, goal::GoalControl, session::SessionController, work_plan::WorkPlan,
};

pub(super) struct RecoveredRegistries {
    pub workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    pub attachments: BTreeMap<AttachmentId, StoredAttachment>,
    pub sessions: BTreeMap<SessionId, Arc<SessionController>>,
    pub child_tasks: BTreeMap<ChildTaskId, StoredChildTask>,
}

pub(super) fn recover_registries(
    recovered: RecoveredRuntime,
) -> RuntimeResult<RecoveredRegistries> {
    let mut workspaces = BTreeMap::new();
    for workspace in recovered.workspaces {
        if workspaces
            .insert(workspace.workspace_id.clone(), workspace)
            .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    let mut input_sessions = BTreeMap::<InputId, SessionId>::new();
    for input in &recovered.inputs {
        if input_sessions
            .insert(input.input_id.clone(), input.session_id.clone())
            .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    let mut stored_goals_by_session = BTreeMap::new();
    for goal in &recovered.goals {
        if stored_goals_by_session
            .insert(goal.session_id.clone(), goal)
            .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    let mut queued_goal_inputs = BTreeMap::<SessionId, usize>::new();
    for input in &recovered.inputs {
        if input.state == crate::StoredInputState::Queued
            && let Some(binding) = input.goal_binding.as_ref()
        {
            let Some(goal) = stored_goals_by_session.get(&input.session_id) else {
                return Err(invalid_recovery());
            };
            if binding.goal_id != goal.goal_id || binding.generation > goal.generation {
                return Err(invalid_recovery());
            }
            let count = queued_goal_inputs
                .entry(input.session_id.clone())
                .or_default();
            *count += 1;
            if *count > 1 {
                return Err(invalid_recovery());
            }
        }
    }
    let mut referenced_inputs = std::collections::BTreeSet::new();
    let mut run_sessions = BTreeMap::<RunId, SessionId>::new();
    for run in &recovered.runs {
        if input_sessions.get(&run.input_id) != Some(&run.session_id) {
            return Err(invalid_recovery());
        }
        referenced_inputs.insert(run.input_id.clone());
        if run_sessions
            .insert(run.run_id.clone(), run.session_id.clone())
            .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    if input_sessions
        .keys()
        .any(|input_id| !referenced_inputs.contains(input_id))
    {
        return Err(invalid_recovery());
    }
    let mut runs_by_session = BTreeMap::<SessionId, Vec<_>>::new();
    for run in recovered.runs {
        runs_by_session
            .entry(run.session_id.clone())
            .or_default()
            .push(run);
    }
    let mut inputs_by_session = BTreeMap::<SessionId, Vec<_>>::new();
    for input in recovered.inputs {
        inputs_by_session
            .entry(input.session_id.clone())
            .or_default()
            .push(input);
    }
    let mut work_plans_by_session = BTreeMap::<SessionId, WorkPlan>::new();
    for stored in recovered.work_plans {
        let session_id = stored.session_id.clone();
        let plan = WorkPlan::try_from(stored).map_err(|_| invalid_recovery())?;
        if work_plans_by_session.insert(session_id, plan).is_some() {
            return Err(invalid_recovery());
        }
    }
    let mut goals_by_session = BTreeMap::<SessionId, GoalControl>::new();
    for stored in recovered.goals {
        let session_id = stored.session_id.clone();
        let goal = GoalControl::try_from(stored).map_err(|_| invalid_recovery())?;
        if goals_by_session.insert(session_id, goal).is_some() {
            return Err(invalid_recovery());
        }
    }

    let mut sessions = BTreeMap::new();
    for stored in recovered.sessions {
        if let Some(workspace_id) = stored.environment.workspace_id.as_ref()
            && !workspaces.contains_key(workspace_id)
        {
            return Err(invalid_recovery());
        }
        let session_id = stored.session_id.clone();
        let controller = Arc::new(SessionController::recovered(
            stored,
            runs_by_session.remove(&session_id).unwrap_or_default(),
            inputs_by_session.remove(&session_id).unwrap_or_default(),
            work_plans_by_session.remove(&session_id),
            goals_by_session.remove(&session_id),
        ));
        if sessions.insert(session_id, controller).is_some() {
            return Err(invalid_recovery());
        }
    }
    if !runs_by_session.is_empty()
        || !inputs_by_session.is_empty()
        || !work_plans_by_session.is_empty()
        || !goals_by_session.is_empty()
    {
        return Err(invalid_recovery());
    }
    let mut child_tasks = BTreeMap::new();
    for child in recovered.child_tasks {
        if !sessions.contains_key(&child.session_id)
            || run_sessions.get(&child.parent_run_id) != Some(&child.session_id)
            || child_tasks
                .insert(child.child_task_id.clone(), child)
                .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    let mut attachments = BTreeMap::new();
    let mut attachment_keys = std::collections::BTreeSet::new();
    for attachment in recovered.attachments {
        if !sessions.contains_key(&attachment.session_id)
            || !attachment_keys
                .insert((attachment.session_id.clone(), attachment.blob_hash.clone()))
            || attachments
                .insert(attachment.attachment_id.clone(), attachment)
                .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    Ok(RecoveredRegistries {
        workspaces,
        attachments,
        sessions,
        child_tasks,
    })
}

fn invalid_recovery() -> RuntimeError {
    RuntimeError::StorageUnavailable {
        operation: "recover runtime",
        source: None,
    }
}
