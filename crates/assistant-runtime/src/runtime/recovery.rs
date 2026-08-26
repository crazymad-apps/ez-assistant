//! Store 启动投影到 Runtime Session registry 的唯一转换。

use std::{collections::BTreeMap, sync::Arc};

use assistant_protocol::{
    AttachmentId, ChildTaskId, InputId, RunId, RunStatus, SessionId, WorkspaceId,
};

use crate::{
    RecoveredRuntime, RuntimeError, RuntimeResult, StoredAttachment, StoredChildTask,
    StoredInputState, StoredRunSettlement, StoredSessionLifecycle, StoredWorkspace,
    goal::GoalControl, session::SessionController, work_plan::WorkPlan,
};

use super::controller::{ProxyReportDraft, build_proxy_report_input};

pub(super) struct RecoveredRegistries {
    pub workspaces: BTreeMap<WorkspaceId, StoredWorkspace>,
    pub attachments: BTreeMap<AttachmentId, StoredAttachment>,
    pub sessions: BTreeMap<SessionId, Arc<SessionController>>,
    pub child_tasks: BTreeMap<ChildTaskId, StoredChildTask>,
}

pub(super) fn prepare_interrupted_run_settlements(
    recovered: &RecoveredRuntime,
    finished_at_ms: i64,
) -> RuntimeResult<Vec<StoredRunSettlement>> {
    let sessions = recovered
        .sessions
        .iter()
        .map(|session| (session.session_id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let inputs = recovered
        .inputs
        .iter()
        .map(|input| (input.input_id.clone(), input))
        .collect::<BTreeMap<_, _>>();
    let mut settlements = Vec::new();
    for run in &recovered.runs {
        let Some(input) = inputs.get(&run.input_id).copied() else {
            continue;
        };
        let recoverable = matches!(run.status, RunStatus::Running | RunStatus::Cancelling)
            || run.status == RunStatus::Accepted && input.state == StoredInputState::Committed;
        if !recoverable {
            continue;
        }
        let source = sessions
            .get(&run.session_id)
            .copied()
            .ok_or_else(invalid_recovery)?;
        let source_queue_empty = !recovered.inputs.iter().any(|candidate| {
            candidate.session_id == run.session_id && candidate.state == StoredInputState::Queued
        });
        let proxy_report = if source.role == crate::SessionRole::Standard && source_queue_empty {
            source
                .proxy
                .as_ref()
                .and_then(|proxy| sessions.get(&proxy.controller_session_id).copied())
                .filter(|controller| {
                    controller.role == crate::SessionRole::Controller
                        && controller.lifecycle == StoredSessionLifecycle::Active
                })
                .map(|controller| {
                    let draft = ProxyReportDraft {
                        source_session_id: source.session_id.clone(),
                        source_title: source.title.clone(),
                        source_run_id: run.run_id.clone(),
                        source_goal_id: input
                            .goal_binding
                            .as_ref()
                            .map(|binding| binding.goal_id.clone()),
                        goal_summary: recovered
                            .goals
                            .iter()
                            .find(|goal| goal.session_id == source.session_id)
                            .map(|goal| {
                                format!(
                                    "state={:?}, runs={}/{}, tokens={}/{}, usage_complete={}",
                                    goal.state,
                                    goal.budget.used_runs,
                                    goal.budget.max_runs,
                                    goal.budget.used_total_tokens,
                                    goal.budget.max_total_tokens,
                                    goal.budget.usage_complete,
                                )
                            }),
                        source_run_status: RunStatus::Interrupted,
                        controller_session_id: controller.session_id.clone(),
                        final_text: None,
                        error: None,
                        accepted_at_ms: finished_at_ms,
                    };
                    build_proxy_report_input(
                        draft,
                        controller.current_variant,
                        controller.approval_mode,
                        recovery_input_id()?,
                        recovery_run_id()?,
                    )
                    .map(Box::new)
                })
                .transpose()?
        } else {
            None
        };
        settlements.push(StoredRunSettlement {
            operation_id: crate::id::generate("recovery-settlement").map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "recovery settlement operation id",
                }
            })?,
            run_id: run.run_id.clone(),
            session_id: run.session_id.clone(),
            status: RunStatus::Interrupted,
            cancel_requested: run.cancel_requested,
            error: None,
            messages: Vec::new(),
            message_step: None,
            goal_effect: None,
            proxy_report,
            finished_at_ms,
        });
    }
    Ok(settlements)
}

fn recovery_input_id() -> RuntimeResult<InputId> {
    let value =
        crate::id::generate("input").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "recovery proxy report input id",
        })?;
    InputId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "recovery proxy report input id",
    })
}

fn recovery_run_id() -> RuntimeResult<RunId> {
    let value = crate::id::generate("run").map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "recovery proxy report Run id",
    })?;
    RunId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "recovery proxy report Run id",
    })
}

pub(super) fn recover_registries(
    recovered: RecoveredRuntime,
) -> RuntimeResult<RecoveredRegistries> {
    let session_roles = recovered
        .sessions
        .iter()
        .map(|session| {
            (
                session.session_id.clone(),
                (session.role, session.lifecycle),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if session_roles.len() != recovered.sessions.len()
        || recovered.sessions.iter().any(|session| {
            session.role == crate::SessionRole::Controller
                && (session.proxy.is_some()
                    || session.lifecycle != crate::StoredSessionLifecycle::Active)
                || session.proxy.as_ref().is_some_and(|proxy| {
                    session.role != crate::SessionRole::Standard
                        || proxy.controller_session_id == session.session_id
                        || session_roles.get(&proxy.controller_session_id)
                            != Some(&(
                                crate::SessionRole::Controller,
                                crate::StoredSessionLifecycle::Active,
                            ))
                })
        })
    {
        return Err(invalid_recovery());
    }
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
    let mut input_activations = BTreeMap::new();
    for input in &recovered.inputs {
        let target_role = session_roles.get(&input.session_id).copied();
        let source_is_valid = match (&input.origin, &input.cross_session_binding) {
            (crate::InputOrigin::User, None) => true,
            (crate::InputOrigin::Runtime, None) => input.goal_binding.is_some(),
            (
                crate::InputOrigin::Runtime,
                Some(crate::CrossSessionInputBinding::ControllerDelivery {
                    controller_session_id,
                    ..
                }),
            ) => {
                input.goal_binding.is_none()
                    && target_role.is_some_and(|(role, _)| role == crate::SessionRole::Standard)
                    && session_roles.get(controller_session_id)
                        == Some(&(
                            crate::SessionRole::Controller,
                            crate::StoredSessionLifecycle::Active,
                        ))
            }
            (
                crate::InputOrigin::Runtime,
                Some(crate::CrossSessionInputBinding::ProxyReport { .. }),
            ) => {
                input.goal_binding.is_none()
                    && target_role
                        == Some((
                            crate::SessionRole::Controller,
                            crate::StoredSessionLifecycle::Active,
                        ))
            }
            (crate::InputOrigin::User, Some(_)) => false,
        };
        if !source_is_valid {
            return Err(invalid_recovery());
        }
        if input_sessions
            .insert(input.input_id.clone(), input.session_id.clone())
            .is_some()
        {
            return Err(invalid_recovery());
        }
        if let Some(activation) = input.skill_activation.as_ref()
            && input_activations
                .insert(activation.activation_id.clone(), activation.clone())
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
    let mut ledger_activation_ids = std::collections::BTreeSet::new();
    let mut activations_by_session = BTreeMap::<SessionId, Vec<_>>::new();
    for activation in recovered.skill_activations {
        if !ledger_activation_ids.insert(activation.activation_id.clone())
            || activation.input_id.as_ref().is_some_and(|input_id| {
                input_sessions.get(input_id) != Some(&activation.session_id)
            })
            || activation
                .run_id
                .as_ref()
                .is_some_and(|run_id| run_sessions.get(run_id) != Some(&activation.session_id))
            || activation.input_id.is_some()
                && input_activations.get(&activation.activation_id) != Some(&activation)
        {
            return Err(invalid_recovery());
        }
        activations_by_session
            .entry(activation.session_id.clone())
            .or_default()
            .push(activation);
    }
    if input_activations
        .keys()
        .any(|activation_id| !ledger_activation_ids.contains(activation_id))
    {
        return Err(invalid_recovery());
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

    let recovered_children = recovered
        .child_tasks
        .iter()
        .map(|child| {
            (
                child.child_task_id.as_str().to_owned(),
                child.session_id.clone(),
                child.parent_run_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut sessions = BTreeMap::new();
    for stored in recovered.sessions {
        if let Some(workspace_id) = stored.environment.workspace_id.as_ref()
            && !workspaces.contains_key(workspace_id)
        {
            return Err(invalid_recovery());
        }
        let session_id = stored.session_id.clone();
        let activations = activations_by_session
            .remove(&session_id)
            .unwrap_or_default();
        if activations.iter().any(|activation| {
            activation.catalog_revision != stored.skill_catalog.revision
                || stored.skill_catalog.definitions.iter().all(|definition| {
                    definition.name != activation.name
                        || definition.definition_digest != activation.definition_digest
                })
                || match &activation.owner {
                    crate::SkillActivationOwner::Session(owner) => owner != &session_id,
                    crate::SkillActivationOwner::ChildTask(owner) => !recovered_children
                        .iter()
                        .any(|(child_id, child_session_id, parent_run_id)| {
                            child_id == owner
                                && child_session_id == &session_id
                                && activation.run_id.as_ref() == Some(parent_run_id)
                        }),
                }
        }) {
            return Err(invalid_recovery());
        }
        let controller = Arc::new(SessionController::recovered(
            stored,
            runs_by_session.remove(&session_id).unwrap_or_default(),
            inputs_by_session.remove(&session_id).unwrap_or_default(),
            work_plans_by_session.remove(&session_id),
            goals_by_session.remove(&session_id),
            activations,
        ));
        if sessions.insert(session_id, controller).is_some() {
            return Err(invalid_recovery());
        }
    }
    if !runs_by_session.is_empty()
        || !inputs_by_session.is_empty()
        || !work_plans_by_session.is_empty()
        || !goals_by_session.is_empty()
        || !activations_by_session.is_empty()
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
