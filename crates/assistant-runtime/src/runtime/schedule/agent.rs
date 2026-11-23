//! Agent 计划使用独立会话，队列和后台监督者直接向本次执行报告完成事实。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use agent_types::{AssistantPart, ConversationMessage, ConversationSnapshot};
use assistant_protocol::{
    AgentVariant, IdempotencyKey, RunStatus, ScheduleAction, ScheduleAgentResult,
    ScheduleBackgroundExit, ScheduleExecutionId, ScheduleExecutionReason as Reason,
    ScheduleExecutionStatus as Status, ScheduleSettings, SessionId, SessionMaterializationManifest,
    SubmitInputMode,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{AssistantRuntime, RuntimeError, RuntimeResult, session::SessionController};

const SESSION_PREFIX: &str = "scheduled_session_";
pub(in crate::runtime) const MATERIALIZATION_KEY_PREFIX: &str = "scheduled-materialization-";

pub(crate) fn is_scheduled_session(id: &SessionId) -> bool {
    id.as_str().starts_with(SESSION_PREFIX)
}

pub(in crate::runtime) fn agent_visibility(id: &SessionId) -> RuntimeResult<()> {
    if is_scheduled_session(id) {
        return Err(RuntimeError::SessionNotFound {
            session_id: id.clone(),
        });
    }
    Ok(())
}

pub(crate) struct ScheduledSession {
    pub(crate) execution_id: ScheduleExecutionId,
    pub(crate) cancellation: CancellationToken,
    pub(crate) approval_required: AtomicBool,
    pub(crate) queue_finished: watch::Sender<bool>,
    pub(in crate::runtime) background_changed: watch::Sender<()>,
    pub(in crate::runtime) background_exits: Mutex<Vec<ScheduleBackgroundExit>>,
}

impl ScheduledSession {
    fn new(execution_id: ScheduleExecutionId, cancellation: CancellationToken) -> Self {
        Self {
            execution_id,
            cancellation,
            approval_required: AtomicBool::new(false),
            queue_finished: watch::channel(false).0,
            background_changed: watch::channel(()).0,
            background_exits: Mutex::new(Vec::new()),
        }
    }

    pub(in crate::runtime) fn session_id(&self) -> RuntimeResult<SessionId> {
        SessionId::new(format!("{SESSION_PREFIX}{}", self.execution_id.as_str())).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "scheduled session id",
            }
        })
    }
}

impl AssistantRuntime {
    pub(super) async fn execute_scheduled_agent(
        &self,
        execution_id: ScheduleExecutionId,
        settings: ScheduleSettings,
        cancellation: CancellationToken,
    ) -> (Status, ScheduleAgentResult) {
        let ScheduleAction::Agent {
            instructions,
            custom_agent,
            model_selection,
            approval_mode,
        } = settings.configuration.action
        else {
            return result(Status::Failed, Reason::AgentPreparationFailed);
        };
        let scheduled = Arc::new(ScheduledSession::new(
            execution_id.clone(),
            cancellation.clone(),
        ));
        let Ok(idempotency_key) = IdempotencyKey::new(format!(
            "{MATERIALIZATION_KEY_PREFIX}{}",
            execution_id.as_str()
        )) else {
            return result(Status::Failed, Reason::AgentPreparationFailed);
        };
        let manifest = SessionMaterializationManifest {
            idempotency_key,
            workspace_id: settings.configuration.workspace_id,
            custom_agent,
            model_selection,
            reasoning_effort: None,
            variant: AgentVariant::Build,
            approval_mode,
            message: instructions,
            mode: SubmitInputMode::Normal,
            attachments: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
        };
        // 与关机串行直到物化完成并注册队列。等待 Agent 和后台时不持生命周期门禁。
        let materialized = {
            let _operation = tokio::select! {
                biased;
                () = cancellation.cancelled() => return cancelled(),
                gate = self.operation_gate.read() => gate,
            };
            if self.ensure_running().is_err() || cancellation.is_cancelled() {
                return cancelled();
            }
            self.materialize_session_inner(manifest, Vec::new(), Some(scheduled.clone()))
                .await
        };
        let session_id = match materialized {
            Ok(materialized) => materialized.session.session_id,
            Err(error) => {
                eprintln!(
                    "event=scheduled_agent_materialization_failed execution_id={execution_id} error={error}"
                );
                // 事务可能已经提交，只有确认没有绑定时才能归类为准备失败。
                return match self.store.get_schedule_execution(execution_id).await {
                    Ok(Some(execution)) if execution.agent_session_id.is_none() => {
                        if cancellation.is_cancelled() {
                            cancelled()
                        } else {
                            result(Status::Failed, Reason::AgentPreparationFailed)
                        }
                    }
                    _ => result(Status::OutcomeUnknown, Reason::AgentPreparationFailed),
                };
            }
        };
        let Ok(Some(session)) = self.session_loader.cached(&session_id) else {
            return result(Status::OutcomeUnknown, Reason::AgentPreparationFailed);
        };
        let mut queue = scheduled.queue_finished.subscribe();
        if queue.wait_for(|finished| *finished).await.is_err() {
            return result(Status::OutcomeUnknown, Reason::AgentRunFailed);
        }
        let background_confirmed = self
            .wait_for_scheduled_background(&scheduled, &session_id)
            .await;
        let outcome = self
            .scheduled_agent_result(&session, &scheduled, background_confirmed)
            .await;
        if !outcome.0.occupies_slot() {
            // 可靠会话仍可供交付读取，完成后释放运行期间的内存投影。
            if let Err(error) = self.child_tasks.remove_session(&session_id) {
                eprintln!(
                    "event=scheduled_child_projection_cleanup_failed session_id={session_id} error={error}"
                );
            }
            if let Ok(mut sessions) = self.sessions.write() {
                sessions.remove(&session_id);
            }
        }
        outcome
    }

    async fn wait_for_scheduled_background(
        &self,
        scheduled: &ScheduledSession,
        id: &SessionId,
    ) -> bool {
        let mut changed = scheduled.background_changed.subscribe();
        loop {
            if !self.background_runs.has_session(id) {
                return true;
            }
            if self.background_runs.session_cleanup_unconfirmed(id) {
                return false;
            }
            tokio::select! {
                () = scheduled.cancellation.cancelled() => {
                    self.background_runs.request_session_stop(id);
                    return tokio::time::timeout(self.config.shutdown_timeout, async {
                        while self.background_runs.has_session(id) {
                            if changed.changed().await.is_err() {
                                return false;
                            }
                        }
                        true
                    }).await.unwrap_or(false);
                }
                update = changed.changed() => {
                    if update.is_err() {
                        return false;
                    }
                }
            }
        }
    }

    async fn scheduled_agent_result(
        &self,
        session: &SessionController,
        scheduled: &ScheduledSession,
        background_confirmed: bool,
    ) -> (Status, ScheduleAgentResult) {
        let run_status = session
            .run_snapshots()
            .ok()
            .and_then(|runs| runs.first().map(|run| run.status));
        let background_exits = scheduled
            .background_exits
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let mut result = ScheduleAgentResult {
            run_status,
            background_exits,
            ..Default::default()
        };
        let run_cleanup_confirmed = session.lock_state().is_ok_and(|state| {
            !state.is_faulted && !state.tool_cleanup_unconfirmed && state.active_run.is_none()
        });
        let (status, reason) = if !background_confirmed || !run_cleanup_confirmed {
            (Status::CleanupUnconfirmed, Some(Reason::CleanupUnconfirmed))
        } else if scheduled.cancellation.is_cancelled() || run_status == Some(RunStatus::Cancelled)
        {
            (Status::Cancelled, Some(Reason::Cancelled))
        } else if scheduled.approval_required.load(Ordering::Acquire) {
            (Status::Failed, Some(Reason::AgentApprovalRequired))
        } else if result
            .background_exits
            .iter()
            .any(|exit| exit.exit_code.is_none() && !exit.stopped && !exit.timed_out)
        {
            (Status::OutcomeUnknown, Some(Reason::BackgroundFailed))
        } else if run_status != Some(RunStatus::Completed) {
            (Status::Failed, Some(Reason::AgentRunFailed))
        } else if result
            .background_exits
            .iter()
            .any(|exit| exit.exit_code != Some(0) || exit.stopped || exit.timed_out)
        {
            (Status::Failed, Some(Reason::BackgroundFailed))
        } else {
            (Status::Succeeded, None)
        };
        result.reason = reason;
        if run_status == Some(RunStatus::Completed) {
            match self.store.load_conversation(session.id()).await {
                Ok(conversation) => {
                    result.report = final_report(&conversation);
                    result.artifacts = crate::artifact::project_artifacts(&conversation);
                }
                Err(_) if status == Status::Succeeded => {
                    result.reason = Some(Reason::AgentRunFailed);
                    return (Status::OutcomeUnknown, result);
                }
                Err(_) => {}
            }
        }
        (status, result)
    }
}

fn final_report(conversation: &ConversationSnapshot) -> Option<String> {
    let ConversationMessage::Assistant(message) = conversation.messages.last()? else {
        return None;
    };
    if !message.purpose.is_model_response()
        || message
            .parts
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
    {
        return None;
    }
    let report: String = message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect();
    (!report.trim().is_empty()).then_some(report)
}

fn result(status: Status, reason: Reason) -> (Status, ScheduleAgentResult) {
    (
        status,
        ScheduleAgentResult {
            reason: Some(reason),
            ..Default::default()
        },
    )
}

fn cancelled() -> (Status, ScheduleAgentResult) {
    result(Status::Cancelled, Reason::Cancelled)
}
