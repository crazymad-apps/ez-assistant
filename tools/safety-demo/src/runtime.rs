//! Safety Demo 的单 Session、Run、审批与临时工作区权威状态。

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use agent_core::{AgentEvent, AgentExecution, ExecutionContext, ExecutionInput, ExecutionOutcome};
use agent_tools::AbsolutePath;
use agent_types::{MessageId, PartId, TextPart, UserMessage, UserPart};
use futures_util::StreamExt;
use tempfile::TempDir;
use thiserror::Error;
use tokio::sync::{Mutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{
        ApprovalCoordinator, ApprovalDecision, ApprovalError, DemoApprovalAuthorizer,
        StateChangeNotifier,
    },
    audit::DemoAudit,
    journal::{DemoJournal, DemoRecorder},
    policy::{ExecutionMode, build_authorizer, plan_authorizer},
    resources::DemoResources,
    wire::{
        EventKind, EventNotification, GuardrailCheckSnapshot, GuardrailSettingsSnapshot,
        GuardrailTriggerSnapshot, RunProgressDetail, RunSnapshot, RunStatus, SessionSnapshot,
        StartRunRequest,
    },
};

const EVENT_CAPACITY: usize = 128;

trait WorkspaceLifecycle: Send + Sync {
    fn create(&self) -> io::Result<TempDir>;
    fn close(&self, workspace: TempDir) -> io::Result<()>;
}

#[derive(Debug)]
struct SystemWorkspaceLifecycle;

impl WorkspaceLifecycle for SystemWorkspaceLifecycle {
    fn create(&self) -> io::Result<TempDir> {
        tempfile::Builder::new()
            .prefix("ez-assistant-safety-demo-")
            .tempdir()
    }

    fn close(&self, workspace: TempDir) -> io::Result<()> {
        workspace.close()
    }
}

struct TemporaryWorkspace {
    directory: TempDir,
    logical_path: AbsolutePath,
}

impl TemporaryWorkspace {
    fn new(directory: TempDir) -> Result<Self, WorkspaceError> {
        let logical_path = AbsolutePath::new(directory.path().to_path_buf())
            .map_err(|_| WorkspaceError::NonUtf8Path)?;
        Ok(Self {
            directory,
            logical_path,
        })
    }
}

struct DemoRun {
    snapshot: RunSnapshot,
    cancellation: CancellationToken,
}

impl DemoRun {
    fn is_active(&self) -> bool {
        self.snapshot.status == RunStatus::Running
    }
}

struct DemoSession {
    session_id: DemoSessionId,
    session_workdir: AbsolutePath,
    temporary_workspace: Option<TemporaryWorkspace>,
    run: Option<DemoRun>,
    is_resetting: bool,
    next_run: u64,
    #[cfg(test)]
    busy_override: bool,
}

impl DemoSession {
    fn has_active_run(&self) -> bool {
        self.run.as_ref().is_some_and(DemoRun::is_active)
    }
}

/// Demo 进程内的私有 Session ID；不上提为 Runtime 或应用协议类型。
struct DemoSessionId(String);

impl DemoSessionId {
    fn single_process_session() -> Self {
        Self("session-1".to_owned())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

struct EventHub {
    sender: broadcast::Sender<EventNotification>,
    sequence: AtomicU64,
}

impl EventHub {
    fn new() -> Arc<Self> {
        let (sender, _receiver) = broadcast::channel(EVENT_CAPACITY);
        Arc::new(Self {
            sender,
            sequence: AtomicU64::new(0),
        })
    }

    fn publish(&self, kind: EventKind) {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.sender.send(EventNotification { sequence, kind });
    }

    fn current(&self) -> u64 {
        self.sequence.load(Ordering::SeqCst)
    }
}

/// HTTP handlers 共享的 Demo 权威状态句柄。
#[derive(Clone)]
pub(crate) struct DemoRuntime {
    session: Arc<Mutex<DemoSession>>,
    lifecycle: Arc<dyn WorkspaceLifecycle>,
    resources: Arc<DemoResources>,
    journal: DemoJournal,
    audit: DemoAudit,
    approvals: ApprovalCoordinator,
    events: Arc<EventHub>,
    run_finished: Arc<Notify>,
}

impl DemoRuntime {
    pub(crate) async fn new_with_resources(
        session_workdir: AbsolutePath,
        resources: Arc<DemoResources>,
    ) -> Result<Self, WorkspaceError> {
        Self::new_with_lifecycle(
            session_workdir,
            resources,
            Arc::new(SystemWorkspaceLifecycle),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn new(session_workdir: AbsolutePath) -> Result<Self, WorkspaceError> {
        let model = Arc::new(agent_testkit::ScriptedModelService::new(
            agent_model::ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            128_000,
            [],
        ));
        let resources = DemoResources::with_model(&session_workdir, model)
            .expect("test resources must be valid");
        Self::new_with_resources(session_workdir, resources).await
    }

    async fn new_with_lifecycle(
        session_workdir: AbsolutePath,
        resources: Arc<DemoResources>,
        lifecycle: Arc<dyn WorkspaceLifecycle>,
    ) -> Result<Self, WorkspaceError> {
        let temporary_workspace = create_workspace(lifecycle.clone()).await.map(Some)?;
        Ok(Self {
            session: Arc::new(Mutex::new(DemoSession {
                session_id: DemoSessionId::single_process_session(),
                session_workdir,
                temporary_workspace,
                run: None,
                is_resetting: false,
                next_run: 1,
                #[cfg(test)]
                busy_override: false,
            })),
            lifecycle,
            resources,
            journal: DemoJournal::default(),
            audit: DemoAudit::default(),
            approvals: ApprovalCoordinator::default(),
            events: EventHub::new(),
            run_finished: Arc::new(Notify::new()),
        })
    }

    pub(crate) async fn snapshot(&self) -> SessionSnapshot {
        let approval = self.approvals.snapshot();
        let (session_id, session_workdir, temporary_workspace, run, is_resetting) = {
            let session = self.session.lock().await;
            (
                session.session_id.as_str().to_owned(),
                session.session_workdir.to_string(),
                session
                    .temporary_workspace
                    .as_ref()
                    .map_or_else(String::new, |workspace| workspace.logical_path.to_string()),
                session.run.as_ref().map(|run| run.snapshot.clone()),
                session.is_resetting,
            )
        };
        let journal = self.journal.snapshot().messages;
        let audit = self.audit.entries();
        SessionSnapshot {
            session_id,
            session_workdir,
            temporary_workspace,
            active_run: run
                .as_ref()
                .is_some_and(|run| run.status == RunStatus::Running),
            run,
            pending_approval: approval.is_some(),
            approval,
            is_resetting,
            journal_entries: journal.len(),
            journal,
            audit_entries: audit.len(),
            audit,
            sequence: self.events.current(),
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<EventNotification> {
        self.events.sender.subscribe()
    }

    pub(crate) async fn start_run(
        &self,
        request: StartRunRequest,
    ) -> Result<SessionSnapshot, StartRunError> {
        let message = request.message.trim();
        if message.is_empty() {
            return Err(StartRunError::EmptyMessage);
        }
        if self.journal.has_pending() || self.approvals.has_pending() {
            return Err(StartRunError::Busy);
        }

        let cancellation = CancellationToken::new();
        let (run_id, session_workdir, temporary_workspace) = {
            let mut session = self.session.lock().await;
            if session.is_resetting || session.has_active_run() {
                return Err(StartRunError::Busy);
            }
            let temporary_workspace = session
                .temporary_workspace
                .as_ref()
                .ok_or(StartRunError::WorkspaceUnavailable)?
                .logical_path
                .clone();
            let run_id = format!("run-{}", session.next_run);
            session.next_run = session.next_run.saturating_add(1);
            let user = user_message(&run_id, message)?;
            self.journal.append_user(user);
            session.run = Some(DemoRun {
                snapshot: RunSnapshot {
                    run_id: run_id.clone(),
                    status: RunStatus::Running,
                    execution_mode: request.execution_mode,
                    approval_mode: request.approval_mode,
                    cancel_requested: false,
                    event_count: 0,
                    guardrails: GuardrailSettingsSnapshot {
                        repeated_invocation: GuardrailCheckSnapshot {
                            mode: agent_core::ActiveGuardrailMode::Observe,
                            threshold: 3,
                        },
                        consecutive_failures: GuardrailCheckSnapshot {
                            mode: agent_core::ActiveGuardrailMode::Enforce,
                            threshold: 5,
                        },
                    },
                    last_guardrail: None,
                    last_error: None,
                },
                cancellation: cancellation.clone(),
            });
            (run_id, session.session_workdir.clone(), temporary_workspace)
        };

        let notify = self.approval_notifier();
        let approval_authorizer: Arc<dyn agent_core::ToolAuthorizer> =
            Arc::new(DemoApprovalAuthorizer::new(
                run_id.clone(),
                self.approvals.clone(),
                cancellation.clone(),
                self.audit.clone(),
                notify,
            ));
        let authorizer = match request.execution_mode {
            ExecutionMode::Plan => plan_authorizer(
                &run_id,
                session_workdir,
                temporary_workspace,
                self.audit.clone(),
            ),
            ExecutionMode::Build => build_authorizer(
                &run_id,
                session_workdir,
                temporary_workspace,
                request.approval_mode,
                approval_authorizer,
                self.audit.clone(),
            ),
        };
        let recorder = DemoRecorder::new(self.journal.clone(), self.audit.clone(), run_id.clone());
        let execution = AgentExecution::start(
            self.resources.spec(),
            ExecutionInput {
                conversation: self.journal.snapshot(),
            },
            ExecutionContext {
                cancellation,
                recorder,
                authorizer,
            },
        );
        self.events.publish(EventKind::RunStarted {
            run_id: run_id.clone(),
        });
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.supervise(run_id, execution).await;
        });
        Ok(self.snapshot().await)
    }

    async fn supervise(&self, run_id: String, execution: AgentExecution) {
        let mut events = execution.events;
        let observe = async {
            while let Some(event) = events.next().await {
                self.observe_event(&run_id, event).await;
            }
        };
        let (_, outcome) = tokio::join!(observe, execution.completion);
        self.finish_run(&run_id, outcome).await;
    }

    async fn observe_event(&self, run_id: &str, event: AgentEvent) {
        if let AgentEvent::ToolStarted { call_id } = &event {
            self.audit.record_started(run_id, call_id);
        }
        {
            let mut session = self.session.lock().await;
            let Some(run) = session.run.as_mut() else {
                return;
            };
            if run.snapshot.run_id != run_id || !run.is_active() {
                return;
            }
            run.snapshot.event_count = run.snapshot.event_count.saturating_add(1);
            if let AgentEvent::GuardrailTriggered {
                kind,
                mode,
                threshold,
                observed,
                call_id,
            } = &event
            {
                run.snapshot.last_guardrail = Some(GuardrailTriggerSnapshot {
                    kind: *kind,
                    mode: *mode,
                    threshold: threshold.get(),
                    observed: *observed,
                    call_id: call_id.to_string(),
                });
            }
        }
        self.events.publish(EventKind::RunProgress {
            run_id: run_id.to_owned(),
            event: event_name(&event).to_owned(),
            detail: progress_detail(&event),
        });
    }

    async fn finish_run(&self, run_id: &str, outcome: ExecutionOutcome) {
        let (status, last_error) = match &outcome {
            ExecutionOutcome::Completed(message) => {
                self.journal.append_assistant(message.clone());
                (RunStatus::Completed, None)
            }
            ExecutionOutcome::Failed(error) => (RunStatus::Failed, Some(error.to_string())),
            ExecutionOutcome::Cancelled => (RunStatus::Cancelled, None),
            ExecutionOutcome::CompactionRequired { .. } => (RunStatus::CompactionRequired, None),
        };
        self.approvals.clear();
        {
            let mut session = self.session.lock().await;
            let Some(run) = session.run.as_mut() else {
                return;
            };
            if run.snapshot.run_id != run_id || !run.is_active() {
                return;
            }
            run.snapshot.status = status;
            run.snapshot.last_error = last_error;
        }
        self.events.publish(EventKind::RunFinished {
            run_id: run_id.to_owned(),
            status,
        });
        self.run_finished.notify_waiters();
    }

    pub(crate) async fn cancel_run(&self) -> Result<SessionSnapshot, RunControlError> {
        let cancellation = {
            let mut session = self.session.lock().await;
            let run = session.run.as_mut().ok_or(RunControlError::NoActiveRun)?;
            if !run.is_active() {
                if run.snapshot.cancel_requested {
                    None
                } else {
                    return Err(RunControlError::NoActiveRun);
                }
            } else {
                let was_requested = run.snapshot.cancel_requested;
                run.cancellation.cancel();
                run.snapshot.cancel_requested = true;
                (!was_requested).then(|| run.snapshot.run_id.clone())
            }
        };
        if let Some(run_id) = cancellation {
            self.events.publish(EventKind::RunProgress {
                run_id,
                event: "cancel_requested".to_owned(),
                detail: None,
            });
        }
        Ok(self.snapshot().await)
    }

    pub(crate) async fn decide_approval(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<SessionSnapshot, ApprovalError> {
        let pending = self.approvals.decide(approval_id, decision)?;
        if let Ok(call_id) = agent_types::ToolCallId::new(pending.call_id.clone()) {
            self.audit.record_approval_decision(
                &pending.run_id,
                &call_id,
                match decision {
                    ApprovalDecision::AllowOnce => crate::audit::AuditDecision::Allow,
                    ApprovalDecision::Deny => crate::audit::AuditDecision::Deny,
                },
            );
        }
        self.events.publish(EventKind::ApprovalChanged {
            approval_id: Some(approval_id.to_owned()),
        });
        Ok(self.snapshot().await)
    }

    pub(crate) async fn reset(&self) -> Result<SessionSnapshot, ResetError> {
        let has_pending_approval = self.approvals.has_pending();
        {
            let mut session = self.session.lock().await;
            #[cfg(test)]
            let busy_override = session.busy_override;
            #[cfg(not(test))]
            let busy_override = false;
            if session.has_active_run()
                || session.is_resetting
                || busy_override
                || self.journal.has_pending()
                || has_pending_approval
            {
                return Err(ResetError::Busy);
            }
            session.is_resetting = true;
        }

        let replacement = match create_workspace(self.lifecycle.clone()).await {
            Ok(workspace) => workspace,
            Err(error) => {
                self.session.lock().await.is_resetting = false;
                return Err(ResetError::Create(error));
            }
        };

        let previous = {
            let mut session = self.session.lock().await;
            let Some(previous) = session.temporary_workspace.replace(replacement) else {
                session.is_resetting = false;
                return Err(ResetError::Unavailable);
            };
            session.run = None;
            session.is_resetting = false;
            previous
        };
        self.journal.clear();
        self.audit.clear();
        self.approvals.clear();
        self.events.publish(EventKind::SessionReset);
        let snapshot = self.snapshot().await;
        close_workspace(self.lifecycle.clone(), previous.directory)
            .await
            .map_err(ResetError::Cleanup)?;
        Ok(snapshot)
    }

    /// 先取消并等待活动 Run 收敛，再显式关闭当前临时工作区。
    pub(crate) async fn shutdown(&self) -> Result<(), WorkspaceError> {
        loop {
            let notified = self.run_finished.notified();
            let active = {
                let session = self.session.lock().await;
                if let Some(run) = session.run.as_ref().filter(|run| run.is_active()) {
                    run.cancellation.cancel();
                    true
                } else {
                    false
                }
            };
            if !active {
                break;
            }
            notified.await;
        }
        let workspace = self
            .session
            .lock()
            .await
            .temporary_workspace
            .take()
            .map(|workspace| workspace.directory);
        if let Some(workspace) = workspace {
            close_workspace(self.lifecycle.clone(), workspace).await?;
        }
        Ok(())
    }

    fn approval_notifier(&self) -> StateChangeNotifier {
        let events = self.events.clone();
        Arc::new(move || {
            events.publish(EventKind::ApprovalChanged { approval_id: None });
        })
    }

    #[cfg(test)]
    pub(crate) async fn set_busy_for_test(&self, active_run: bool, pending_approval: bool) {
        self.session.lock().await.busy_override = active_run || pending_approval;
    }
}

fn user_message(run_id: &str, text: &str) -> Result<UserMessage, StartRunError> {
    Ok(UserMessage {
        id: MessageId::new(format!("{run_id}-user"))
            .map_err(|error| StartRunError::Identifier(error.to_string()))?,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new(format!("{run_id}-user-text"))
                .map_err(|error| StartRunError::Identifier(error.to_string()))?,
            text: text.to_owned(),
        })],
    })
}

fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::ExecutionStarted => "execution_started",
        AgentEvent::StepStarted { .. } => "step_started",
        AgentEvent::UsageUpdated { .. } => "usage_updated",
        AgentEvent::TextDelta { .. } => "text_delta",
        AgentEvent::ReasoningDelta { .. } => "reasoning_delta",
        AgentEvent::ToolProposed { .. } => "tool_proposed",
        AgentEvent::ToolStarted { .. } => "tool_started",
        AgentEvent::ToolOutput { .. } => "tool_output",
        AgentEvent::ToolCompleted { .. } => "tool_completed",
        AgentEvent::GuardrailTriggered { .. } => "guardrail_triggered",
        AgentEvent::ExecutionCompleted { .. } => "execution_completed",
        AgentEvent::ExecutionFailed { .. } => "execution_failed",
        AgentEvent::ExecutionCancelled { .. } => "execution_cancelled",
        AgentEvent::ExecutionCompactionRequired { .. } => "execution_compaction_required",
    }
}

fn progress_detail(event: &AgentEvent) -> Option<RunProgressDetail> {
    match event {
        AgentEvent::StepStarted { step } => Some(RunProgressDetail::StepStarted { step: *step }),
        AgentEvent::UsageUpdated { step, usage } => Some(RunProgressDetail::UsageUpdated {
            step: *step,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        }),
        AgentEvent::TextDelta { id, delta } => Some(RunProgressDetail::TextDelta {
            part_id: id.to_string(),
            delta: delta.clone(),
        }),
        AgentEvent::ReasoningDelta { id, delta } => Some(RunProgressDetail::ReasoningDelta {
            part_id: id.to_string(),
            delta: delta.clone(),
        }),
        AgentEvent::ToolProposed { call } => Some(RunProgressDetail::ToolProposed {
            call_id: call.id.to_string(),
            tool_name: call.name.as_str().to_owned(),
        }),
        AgentEvent::ToolStarted { call_id } => Some(RunProgressDetail::ToolStarted {
            call_id: call_id.to_string(),
        }),
        AgentEvent::ToolOutput {
            call_id,
            channel,
            chunk,
        } => Some(RunProgressDetail::ToolOutput {
            call_id: call_id.to_string(),
            channel: match channel {
                agent_tools::ToolOutputChannel::Stdout => "stdout",
                agent_tools::ToolOutputChannel::Stderr => "stderr",
            }
            .to_owned(),
            chunk: chunk.clone(),
        }),
        AgentEvent::ToolCompleted { call_id, status } => Some(RunProgressDetail::ToolCompleted {
            call_id: call_id.to_string(),
            status: match status {
                agent_core::ToolCompletionStatus::Success => "success",
                agent_core::ToolCompletionStatus::Failed => "failed",
            }
            .to_owned(),
        }),
        AgentEvent::GuardrailTriggered {
            kind,
            mode,
            threshold,
            observed,
            call_id,
        } => Some(RunProgressDetail::GuardrailTriggered {
            kind: *kind,
            mode: *mode,
            threshold: threshold.get(),
            observed: *observed,
            call_id: call_id.to_string(),
        }),
        AgentEvent::ExecutionStarted
        | AgentEvent::ExecutionCompleted { .. }
        | AgentEvent::ExecutionFailed { .. }
        | AgentEvent::ExecutionCancelled { .. }
        | AgentEvent::ExecutionCompactionRequired { .. } => None,
    }
}

async fn create_workspace(
    lifecycle: Arc<dyn WorkspaceLifecycle>,
) -> Result<TemporaryWorkspace, WorkspaceError> {
    let directory = tokio::task::spawn_blocking(move || lifecycle.create())
        .await
        .map_err(|error| WorkspaceError::Task(error.to_string()))?
        .map_err(|error| WorkspaceError::Io(error.to_string()))?;
    TemporaryWorkspace::new(directory)
}

async fn close_workspace(
    lifecycle: Arc<dyn WorkspaceLifecycle>,
    workspace: TempDir,
) -> Result<(), WorkspaceError> {
    tokio::task::spawn_blocking(move || lifecycle.close(workspace))
        .await
        .map_err(|error| WorkspaceError::Task(error.to_string()))?
        .map_err(|error| WorkspaceError::Io(error.to_string()))
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum StartRunError {
    #[error("message must not be empty")]
    EmptyMessage,
    #[error("another run or pending exchange is active")]
    Busy,
    #[error("temporary workspace is unavailable")]
    WorkspaceUnavailable,
    #[error("failed to create message identifier: {0}")]
    Identifier(String),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum RunControlError {
    #[error("there is no active run")]
    NoActiveRun,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum WorkspaceError {
    #[error("temporary workspace path is not valid UTF-8")]
    NonUtf8Path,
    #[error("temporary workspace operation failed: {0}")]
    Io(String),
    #[error("temporary workspace task failed: {0}")]
    Task(String),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum ResetError {
    #[error("session is busy")]
    Busy,
    #[error("temporary workspace is unavailable")]
    Unavailable,
    #[error("create replacement workspace failed: {0}")]
    Create(WorkspaceError),
    #[error("previous workspace cleanup failed: {0}")]
    Cleanup(WorkspaceError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    };

    use agent_model::ModelCapabilities;
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, ModelIdentity, ProviderId, ToolCall,
        ToolCallId, ToolName,
    };

    use super::*;

    fn workdir(root: &TempDir) -> AbsolutePath {
        AbsolutePath::new(root.path().to_path_buf()).expect("absolute workdir")
    }

    fn tool_turn(call_id: &str, tool_name: &str, arguments: serde_json::Value) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(format!("assistant-{call_id}")).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("provider"),
                "scripted-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new(call_id).expect("call id"),
                name: ToolName::new(tool_name).expect("tool name"),
                arguments,
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    /// 构造同一模型回合内的多个 Tool Call，用于验证批次级 Guardrail 收敛。
    fn tool_batch_turn(
        message_id: &str,
        calls: impl IntoIterator<Item = (&'static str, &'static str, serde_json::Value)>,
    ) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(message_id).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("provider"),
                "scripted-model",
            ),
            parts: calls
                .into_iter()
                .map(|(call_id, tool_name, arguments)| {
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new(call_id).expect("call id"),
                        name: ToolName::new(tool_name).expect("tool name"),
                        arguments,
                    })
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn final_turn(id: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(id).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("provider"),
                "scripted-model",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new(format!("{id}-text")).expect("part id"),
                text: "done".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    async fn runtime_with_turns(root: &TempDir, turns: Vec<AssistantMessage>) -> DemoRuntime {
        let session_workdir = workdir(root);
        let scripts = turns
            .iter()
            .map(agent_testkit::message_events)
            .map(agent_testkit::ModelScript::Events)
            .collect::<Vec<_>>();
        let model = Arc::new(agent_testkit::ScriptedModelService::new(
            ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            128_000,
            scripts,
        ));
        let resources = DemoResources::with_model(&session_workdir, model).expect("resources");
        DemoRuntime::new_with_resources(session_workdir, resources)
            .await
            .expect("runtime")
    }

    async fn wait_for_snapshot(
        runtime: &DemoRuntime,
        predicate: impl Fn(&SessionSnapshot) -> bool,
    ) -> SessionSnapshot {
        let mut events = runtime.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = runtime.snapshot().await;
                if predicate(&snapshot) {
                    return snapshot;
                }
                events.recv().await.expect("runtime event");
            }
        })
        .await
        .expect("runtime reaches expected state")
    }

    fn request(
        execution_mode: ExecutionMode,
        approval_mode: crate::policy::ApprovalMode,
    ) -> StartRunRequest {
        StartRunRequest {
            message: "exercise the requested tool".to_owned(),
            execution_mode,
            approval_mode,
        }
    }

    #[test]
    fn progress_projection_keeps_live_text_but_redacts_tool_arguments() {
        let text = progress_detail(&AgentEvent::TextDelta {
            id: PartId::new("part-live").expect("part id"),
            delta: "streamed text".to_owned(),
        })
        .expect("text progress");
        assert_eq!(
            text,
            RunProgressDetail::TextDelta {
                part_id: "part-live".to_owned(),
                delta: "streamed text".to_owned(),
            }
        );

        let proposed = progress_detail(&AgentEvent::ToolProposed {
            call: ToolCall {
                id: ToolCallId::new("call-redacted").expect("call id"),
                name: ToolName::new("write_file").expect("tool name"),
                arguments: serde_json::json!({"path": "note.txt", "content": "private body"}),
            },
        })
        .expect("tool progress");
        let json = serde_json::to_string(&proposed).expect("serialize progress");
        assert!(json.contains("write_file"));
        assert!(!json.contains("private body"));
        assert!(!json.contains("arguments"));
    }

    #[tokio::test]
    async fn snapshot_is_authoritative_and_reset_advances_sequence() {
        let root = tempfile::tempdir().expect("create workdir");
        let runtime = DemoRuntime::new(workdir(&root))
            .await
            .expect("create demo runtime");
        let initial = runtime.snapshot().await;
        assert_eq!(initial.sequence, 0);
        assert!(!initial.active_run);
        assert!(!initial.pending_approval);
        assert!(std::path::Path::new(&initial.temporary_workspace).is_dir());

        let previous_workspace = initial.temporary_workspace.clone();
        let mut events = runtime.subscribe();
        let reset = runtime.reset().await.expect("reset session");
        let event = events.recv().await.expect("receive reset notification");

        assert_eq!(reset.sequence, 1);
        assert_eq!(event.sequence, reset.sequence);
        assert_eq!(event.kind, EventKind::SessionReset);
        assert_ne!(reset.temporary_workspace, previous_workspace);
        assert!(!std::path::Path::new(&previous_workspace).exists());
        assert_eq!(runtime.snapshot().await, reset);
        runtime.shutdown().await.expect("shutdown runtime");
        assert!(!std::path::Path::new(&reset.temporary_workspace).exists());
    }

    #[tokio::test]
    async fn reset_rejects_active_run_or_pending_approval() {
        let root = tempfile::tempdir().expect("create workdir");
        let runtime = DemoRuntime::new(workdir(&root))
            .await
            .expect("create demo runtime");

        runtime.set_busy_for_test(true, false).await;
        assert_eq!(runtime.reset().await, Err(ResetError::Busy));
        runtime.set_busy_for_test(false, true).await;
        assert_eq!(runtime.reset().await, Err(ResetError::Busy));
        runtime.set_busy_for_test(false, false).await;
        runtime.shutdown().await.expect("shutdown runtime");
    }

    struct FailingCloseLifecycle {
        fail_close: AtomicBool,
    }

    impl WorkspaceLifecycle for FailingCloseLifecycle {
        fn create(&self) -> io::Result<TempDir> {
            tempfile::tempdir()
        }

        fn close(&self, workspace: TempDir) -> io::Result<()> {
            if self.fail_close.swap(false, Ordering::SeqCst) {
                drop(workspace);
                Err(io::Error::other("injected close failure"))
            } else {
                workspace.close()
            }
        }
    }

    /// 把测试预先构造的目录交给 Runtime，便于让脚本模型引用同一个临时工作区。
    struct SeededWorkspaceLifecycle {
        workspace: StdMutex<Option<TempDir>>,
    }

    impl WorkspaceLifecycle for SeededWorkspaceLifecycle {
        fn create(&self) -> io::Result<TempDir> {
            self.workspace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .ok_or_else(|| io::Error::other("seeded workspace was already consumed"))
        }

        fn close(&self, workspace: TempDir) -> io::Result<()> {
            workspace.close()
        }
    }

    #[tokio::test]
    async fn cleanup_failure_is_observable_after_reset_state_commits() {
        let root = tempfile::tempdir().expect("create workdir");
        let workdir = workdir(&root);
        let model = Arc::new(agent_testkit::ScriptedModelService::new(
            agent_model::ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            128_000,
            [],
        ));
        let resources = DemoResources::with_model(&workdir, model).expect("resources");
        let lifecycle = Arc::new(FailingCloseLifecycle {
            fail_close: AtomicBool::new(true),
        });
        let runtime = DemoRuntime::new_with_lifecycle(workdir, resources, lifecycle)
            .await
            .expect("create demo runtime");

        let error = runtime
            .reset()
            .await
            .expect_err("close failure is reported");
        assert!(matches!(error, ResetError::Cleanup(_)));
        assert_eq!(runtime.snapshot().await.sequence, 1);
        runtime.shutdown().await.expect("shutdown runtime");
    }

    #[tokio::test]
    async fn plan_ask_allows_real_read_without_approval() {
        let root = tempfile::tempdir().expect("workdir");
        std::fs::write(root.path().join("note.txt"), "hello\n").expect("write fixture");
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_turn(
                    "call-read",
                    "read_file",
                    serde_json::json!({"path": "note.txt", "offset": 1, "limit": 20}),
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Plan,
                crate::policy::ApprovalMode::Ask,
            ))
            .await
            .expect("start run");
        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(
            snapshot.run.as_ref().map(|run| run.status),
            Some(RunStatus::Completed)
        );
        let guardrails = &snapshot.run.as_ref().expect("run").guardrails;
        assert_eq!(guardrails.repeated_invocation.threshold, 3);
        assert_eq!(
            guardrails.repeated_invocation.mode,
            agent_core::ActiveGuardrailMode::Observe
        );
        assert_eq!(guardrails.consecutive_failures.threshold, 5);
        assert_eq!(
            guardrails.consecutive_failures.mode,
            agent_core::ActiveGuardrailMode::Enforce
        );
        assert!(!snapshot.pending_approval);
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-read"
                && entry.timestamp_ms > 0
                && entry.policy == "plan_capability"
                && entry.status == crate::audit::AuditExecutionStatus::Completed
        }));
        assert_eq!(snapshot.journal.len(), 4);
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn plan_can_write_edit_and_delete_inside_its_real_temporary_workspace() {
        let root = tempfile::tempdir().expect("workdir");
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let workspace_path = workspace.path().to_path_buf();
        let note = workspace_path.join("plan.md");
        let empty_directory = workspace_path.join("empty");
        std::fs::create_dir(&empty_directory).expect("empty directory fixture");

        let turns = [
            tool_turn(
                "call-plan-write",
                "write_file",
                serde_json::json!({"path": note, "content": "draft\n"}),
            ),
            tool_turn(
                "call-plan-edit",
                "edit_file",
                serde_json::json!({
                    "path": note,
                    "old_string": "draft",
                    "new_string": "revised",
                    "replace_all": false,
                }),
            ),
            tool_turn(
                "call-plan-delete-file",
                "delete_file",
                serde_json::json!({"path": note}),
            ),
            tool_turn(
                "call-plan-delete-directory",
                "delete_file",
                serde_json::json!({"path": empty_directory}),
            ),
            final_turn("assistant-final"),
        ];
        let scripts = turns
            .iter()
            .map(agent_testkit::message_events)
            .map(agent_testkit::ModelScript::Events)
            .collect::<Vec<_>>();
        let model = Arc::new(agent_testkit::ScriptedModelService::new(
            ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            128_000,
            scripts,
        ));
        let session_workdir = workdir(&root);
        let resources = DemoResources::with_model(&session_workdir, model).expect("resources");
        let lifecycle = Arc::new(SeededWorkspaceLifecycle {
            workspace: StdMutex::new(Some(workspace)),
        });
        let runtime = DemoRuntime::new_with_lifecycle(session_workdir, resources, lifecycle)
            .await
            .expect("runtime");

        assert_eq!(
            runtime.snapshot().await.temporary_workspace,
            workspace_path.to_string_lossy()
        );
        runtime
            .start_run(request(
                ExecutionMode::Plan,
                crate::policy::ApprovalMode::Ask,
            ))
            .await
            .expect("start run");

        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(
            snapshot.run.as_ref().map(|run| run.status),
            Some(RunStatus::Completed)
        );
        assert!(!note.exists());
        assert!(!empty_directory.exists());
        assert!(!snapshot.pending_approval);
        for call_id in [
            "call-plan-write",
            "call-plan-edit",
            "call-plan-delete-file",
            "call-plan-delete-directory",
        ] {
            assert!(snapshot.audit.iter().any(|entry| {
                entry.call_id == call_id
                    && entry.policy == "plan_capability"
                    && entry.status == crate::audit::AuditExecutionStatus::Completed
            }));
        }

        runtime.shutdown().await.expect("shutdown");
        assert!(!workspace_path.exists());
    }

    #[tokio::test]
    async fn plan_auto_cannot_override_shell_deny() {
        let root = tempfile::tempdir().expect("workdir");
        let marker = root.path().join("must-not-exist.txt");
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_turn(
                    "call-shell-denied",
                    "shell",
                    serde_json::json!({
                        "command": "printf denied > must-not-exist.txt",
                        "workdir": root.path(),
                    }),
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Plan,
                crate::policy::ApprovalMode::Auto,
            ))
            .await
            .expect("start run");
        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(
            snapshot.run.as_ref().map(|run| run.status),
            Some(RunStatus::Completed)
        );
        assert!(!marker.exists());
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-denied"
                && entry.status == crate::audit::AuditExecutionStatus::Denied
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn build_ask_allows_once_and_rejects_second_run_and_repeated_decision() {
        let root = tempfile::tempdir().expect("workdir");
        let marker = root.path().join("approved.txt");
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_turn(
                    "call-shell-ask",
                    "shell",
                    serde_json::json!({
                        "command": "printf approved > approved.txt",
                        "workdir": root.path(),
                    }),
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Build,
                crate::policy::ApprovalMode::Ask,
            ))
            .await
            .expect("start run");
        let pending = wait_for_snapshot(&runtime, |snapshot| snapshot.pending_approval).await;
        assert_eq!(
            runtime
                .start_run(request(
                    ExecutionMode::Plan,
                    crate::policy::ApprovalMode::Auto,
                ))
                .await,
            Err(StartRunError::Busy)
        );
        let approval_id = pending
            .approval
            .as_ref()
            .expect("approval")
            .approval_id
            .clone();
        runtime
            .decide_approval(&approval_id, ApprovalDecision::AllowOnce)
            .await
            .expect("allow once");
        assert_eq!(
            runtime
                .decide_approval(&approval_id, ApprovalDecision::AllowOnce)
                .await,
            Err(ApprovalError::NotPending)
        );
        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(std::fs::read_to_string(marker).expect("marker"), "approved");
        let run = snapshot.run.as_ref().expect("run");
        assert_eq!(run.execution_mode, ExecutionMode::Build);
        assert_eq!(run.approval_mode, crate::policy::ApprovalMode::Ask);
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-ask"
                && entry.decision == Some(crate::audit::AuditDecision::Allow)
                && entry.status == crate::audit::AuditExecutionStatus::Completed
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn build_ask_denial_does_not_execute_real_shell() {
        let root = tempfile::tempdir().expect("workdir");
        let marker = root.path().join("denied.txt");
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_turn(
                    "call-shell-deny",
                    "shell",
                    serde_json::json!({
                        "command": "printf denied > denied.txt",
                        "workdir": root.path(),
                    }),
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Build,
                crate::policy::ApprovalMode::Ask,
            ))
            .await
            .expect("start run");
        let pending = wait_for_snapshot(&runtime, |snapshot| snapshot.pending_approval).await;
        let approval_id = pending.approval.expect("approval").approval_id;
        runtime
            .decide_approval(&approval_id, ApprovalDecision::Deny)
            .await
            .expect("deny once");

        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert!(!marker.exists());
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-deny"
                && entry.decision == Some(crate::audit::AuditDecision::Deny)
                && entry.status == crate::audit::AuditExecutionStatus::Denied
                && entry.error_class.as_deref() == Some("approval_denied")
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn build_auto_executes_unmatched_real_shell_without_approval() {
        let root = tempfile::tempdir().expect("workdir");
        let marker = root.path().join("auto.txt");
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_turn(
                    "call-shell-auto",
                    "shell",
                    serde_json::json!({
                        "command": "printf auto > auto.txt",
                        "workdir": root.path(),
                    }),
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Build,
                crate::policy::ApprovalMode::Auto,
            ))
            .await
            .expect("start run");
        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(std::fs::read_to_string(marker).expect("marker"), "auto");
        assert!(!snapshot.pending_approval);
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-auto" && entry.policy == "auto_allow_all"
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn cancellation_during_approval_is_idempotent_and_cleans_pending_state() {
        let root = tempfile::tempdir().expect("workdir");
        let runtime = runtime_with_turns(
            &root,
            vec![tool_turn(
                "call-shell-cancel",
                "shell",
                serde_json::json!({"command": "pwd", "workdir": root.path()}),
            )],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Build,
                crate::policy::ApprovalMode::Ask,
            ))
            .await
            .expect("start run");
        wait_for_snapshot(&runtime, |snapshot| snapshot.pending_approval).await;
        runtime.cancel_run().await.expect("first cancel");
        runtime.cancel_run().await.expect("idempotent cancel");
        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(
            snapshot.run.as_ref().map(|run| run.status),
            Some(RunStatus::Cancelled)
        );
        assert!(!snapshot.pending_approval);
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-cancel"
                && entry.status == crate::audit::AuditExecutionStatus::Cancelled
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_interrupts_a_running_real_shell_and_marks_audit_cancelled() {
        let root = tempfile::tempdir().expect("workdir");
        let runtime = runtime_with_turns(
            &root,
            vec![tool_turn(
                "call-shell-running",
                "shell",
                serde_json::json!({"command": "sleep 30", "workdir": root.path()}),
            )],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Build,
                crate::policy::ApprovalMode::Auto,
            ))
            .await
            .expect("start run");
        wait_for_snapshot(&runtime, |snapshot| {
            snapshot.audit.iter().any(|entry| {
                entry.call_id == "call-shell-running"
                    && entry.status == crate::audit::AuditExecutionStatus::Started
            })
        })
        .await;
        runtime.cancel_run().await.expect("cancel running shell");

        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        assert_eq!(
            snapshot.run.as_ref().map(|run| run.status),
            Some(RunStatus::Cancelled)
        );
        assert!(snapshot.audit.iter().any(|entry| {
            entry.call_id == "call-shell-running"
                && entry.status == crate::audit::AuditExecutionStatus::Cancelled
                && entry.error_class.as_deref() == Some("cancelled")
        }));
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn repeated_invocation_observe_is_projected_without_stopping_the_run() {
        let root = tempfile::tempdir().expect("workdir");
        let repeated = || serde_json::json!({"command": "pwd", "workdir": root.path()});
        let runtime = runtime_with_turns(
            &root,
            vec![
                tool_batch_turn(
                    "assistant-repeated",
                    [
                        ("call-repeat-1", "shell", repeated()),
                        ("call-repeat-2", "shell", repeated()),
                        ("call-repeat-3", "shell", repeated()),
                    ],
                ),
                final_turn("assistant-final"),
            ],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Plan,
                crate::policy::ApprovalMode::Auto,
            ))
            .await
            .expect("start run");

        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        let run = snapshot.run.expect("run");
        assert_eq!(run.status, RunStatus::Completed);
        let trigger = run.last_guardrail.expect("repeated invocation trigger");
        assert_eq!(trigger.kind, agent_core::GuardrailKind::RepeatedInvocation);
        assert_eq!(trigger.mode, agent_core::ActiveGuardrailMode::Observe);
        assert_eq!(trigger.threshold, 3);
        assert_eq!(trigger.observed, 3);
        assert_eq!(trigger.call_id, "call-repeat-3");
        runtime.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn consecutive_failures_enforce_stops_after_settling_the_batch() {
        let root = tempfile::tempdir().expect("workdir");
        let runtime = runtime_with_turns(
            &root,
            vec![tool_batch_turn(
                "assistant-failures",
                [
                    (
                        "call-failure-1",
                        "shell",
                        serde_json::json!({"command": "echo 1", "workdir": root.path()}),
                    ),
                    (
                        "call-failure-2",
                        "shell",
                        serde_json::json!({"command": "echo 2", "workdir": root.path()}),
                    ),
                    (
                        "call-failure-3",
                        "shell",
                        serde_json::json!({"command": "echo 3", "workdir": root.path()}),
                    ),
                    (
                        "call-failure-4",
                        "shell",
                        serde_json::json!({"command": "echo 4", "workdir": root.path()}),
                    ),
                    (
                        "call-failure-5",
                        "shell",
                        serde_json::json!({"command": "echo 5", "workdir": root.path()}),
                    ),
                ],
            )],
        )
        .await;
        runtime
            .start_run(request(
                ExecutionMode::Plan,
                crate::policy::ApprovalMode::Auto,
            ))
            .await
            .expect("start run");

        let snapshot = wait_for_snapshot(&runtime, |snapshot| !snapshot.active_run).await;
        let run = snapshot.run.expect("run");
        assert_eq!(run.status, RunStatus::Failed);
        assert!(
            run.last_error
                .as_deref()
                .is_some_and(|error| error.contains("ConsecutiveFailures"))
        );
        let trigger = run.last_guardrail.expect("consecutive failure trigger");
        assert_eq!(trigger.kind, agent_core::GuardrailKind::ConsecutiveFailures);
        assert_eq!(trigger.mode, agent_core::ActiveGuardrailMode::Enforce);
        assert_eq!(trigger.threshold, 5);
        assert_eq!(trigger.observed, 5);
        assert_eq!(trigger.call_id, "call-failure-5");
        assert_eq!(snapshot.journal.len(), 7);
        runtime.shutdown().await.expect("shutdown");
    }
}
