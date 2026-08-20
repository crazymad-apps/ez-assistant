//! Core Demo 私有的多 Session 编排、Run gate 与事件 supervisor。

use std::{
    collections::BTreeMap,
    fmt,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicU64, Ordering},
    },
};

use agent_core::{
    ActiveGuardrailMode, AgentEvent, ExecutionContext, ExecutionOutcome, GuardrailCheckConfig,
    GuardrailConfig, ModelRequestConfig,
};
use agent_memory::{
    PinnedMemoryCategory, PinnedMemoryDraft, PinnedMemoryId, PinnedMemoryPatch,
    PinnedMemorySnapshot, PinnedMemorySnapshotInput, PinnedMemoryStore, PinnedMemoryStoreError,
};
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_sdk::{Agent, AgentBuilder, ContextWindowEvaluator, ExecutionInput, ToolAuthorizer};
use agent_tools::{AbsolutePath, ToolOutputChannel, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, MessageId, PartId, TextPart, UserMessage, UserPart,
};
use futures_util::{FutureExt, StreamExt};
use thiserror::Error;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{
        ApprovalCoordinator, ApprovalDecision, ApprovalError, DemoApprovalAuthorizer,
        StateChangeNotifier,
    },
    audit::DemoAudit,
    compaction::CompactionCoordinator,
    config::ServeConfig,
    journal::{DemoJournal, DemoRecorder},
    memory::{
        DEMO_SOURCE_ID, DemoMemoryResources, DemoPinnedStoreSnapshot, FAILING_SOURCE_ID,
        build_memory_resources,
    },
    policy::{build_authorizer, plan_authorizer},
    resources::DemoModelResources,
    tooling::build_tools,
    wire::{
        ConfigSnapshot, CreateSessionRequest, EventKind, EventNotification, ExecutionMode,
        FrozenPromptSummary, GlobalSnapshot, MemoryStoreSnapshot, PinMemoryRequest, RunSnapshot,
        RunStatus, SessionSnapshot, SessionSummary, StartRunRequest, ToolActivitySnapshot,
        UpdateMemoryRequest,
    },
};

const EVENT_CAPACITY: usize = 16;
const MAX_TITLE_CHARS: usize = 80;
const MAX_MESSAGE_CHARS: usize = 16_000;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DemoSessionId(String);

impl fmt::Display for DemoSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DemoRunId(String);

impl fmt::Display for DemoRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone)]
pub(crate) struct DemoRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    config: ServeConfig,
    workdir: AbsolutePath,
    denied_workspace: AbsolutePath,
    tools: ToolSetSnapshot,
    memory: DemoMemoryResources,
    model: Arc<dyn ModelService>,
    model_request: ModelRequestConfig,
    model_provider: String,
    model_name: String,
    model_context_window_tokens: u64,
    model_observation: Arc<crate::resources::ModelObservation>,
    context_window: Arc<ContextWindowEvaluator>,
    compaction: CompactionCoordinator,
    sessions: RwLock<BTreeMap<DemoSessionId, Arc<DemoSession>>>,
    next_session: AtomicU64,
    events: EventHub,
}

struct DemoSession {
    id: DemoSessionId,
    title: String,
    agent: Agent,
    frozen_system_prompt: SystemPromptSnapshot,
    frozen_prompt_summary: FrozenPromptSummary,
    journal: DemoJournal,
    audit: DemoAudit,
    approvals: ApprovalCoordinator,
    _temporary_directory: tempfile::TempDir,
    temporary_workspace: AbsolutePath,
    state: Mutex<DemoSessionState>,
    run_finished: Notify,
    approval_changed: Notify,
}

struct DemoSessionState {
    sequence: u64,
    next_run: u64,
    next_message: u64,
    active_run: Option<ActiveRun>,
    run: Option<RunSnapshot>,
}

struct ActiveRun {
    id: DemoRunId,
    cancellation: CancellationToken,
}

struct EventHub {
    sequence: AtomicU64,
    sender: broadcast::Sender<EventNotification>,
}

impl EventHub {
    fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(EVENT_CAPACITY);
        Self {
            sequence: AtomicU64::new(0),
            sender,
        }
    }

    fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    fn subscribe(&self) -> broadcast::Receiver<EventNotification> {
        self.sender.subscribe()
    }

    fn publish(&self, session_id: &DemoSessionId, session_sequence: u64, kind: EventKind) {
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.sender.send(EventNotification {
            sequence,
            session_id: session_id.to_string(),
            session_sequence,
            kind,
        });
    }
}

impl DemoRuntime {
    pub(crate) async fn new(config: ServeConfig) -> Result<Self, RuntimeError> {
        let model_resources = DemoModelResources::from_environment(config.retry_transient)
            .map_err(|error| RuntimeError::ModelResources(error.to_string()))?;
        Self::with_model_resources(config, model_resources).await
    }

    #[cfg(test)]
    pub(crate) async fn new_offline(config: ServeConfig) -> Result<Self, RuntimeError> {
        let model: Arc<dyn ModelService> = Arc::new(crate::model::DeterministicModel::default());
        Self::with_model_resources(config, DemoModelResources::offline(model)).await
    }

    #[cfg(test)]
    async fn new_offline_with_model(
        config: ServeConfig,
        model: Arc<dyn ModelService>,
    ) -> Result<Self, RuntimeError> {
        Self::with_model_resources(config, DemoModelResources::offline(model)).await
    }

    async fn with_model_resources(
        config: ServeConfig,
        model_resources: DemoModelResources,
    ) -> Result<Self, RuntimeError> {
        let context_window =
            ContextWindowEvaluator::new(0.8).map_err(|_| RuntimeError::InvalidContextWindow)?;
        let workdir = AbsolutePath::new(config.workdir.clone()).map_err(|_| RuntimeError::Path)?;
        let denied_workspace = AbsolutePath::new(workdir.as_path().join(".core-demo-denied"))
            .map_err(|_| RuntimeError::Path)?;
        let memory = build_memory_resources(&config.data_dir)
            .await
            .map_err(|error| RuntimeError::Memory(error.to_string()))?;
        let tools = build_tools(&workdir, &memory).map_err(|_| RuntimeError::Tooling)?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                config,
                workdir,
                denied_workspace,
                tools,
                memory,
                model: model_resources.model,
                model_request: model_resources.model_request,
                model_provider: model_resources.provider,
                model_name: model_resources.model_name,
                model_context_window_tokens: model_resources.context_window_tokens,
                model_observation: model_resources.observation,
                context_window: Arc::new(context_window),
                compaction: CompactionCoordinator::default(),
                sessions: RwLock::new(BTreeMap::new()),
                next_session: AtomicU64::new(0),
                events: EventHub::new(),
            }),
        })
    }

    pub(crate) async fn snapshot(&self) -> GlobalSnapshot {
        let observation = self.inner.model_observation.snapshot();
        let sessions = self
            .sessions()
            .values()
            .map(|session| session.summary())
            .collect();
        GlobalSnapshot {
            sequence: self.inner.events.current_sequence(),
            config: ConfigSnapshot {
                workdir: self.inner.config.workdir.to_string_lossy().into_owned(),
                data_dir: self.inner.config.data_dir.to_string_lossy().into_owned(),
                provider: self.inner.model_provider.clone(),
                model: self.inner.model_name.clone(),
                context_window_tokens: self.inner.model_context_window_tokens,
                reasoning_enabled: self.inner.model_request.reasoning.is_some(),
                retry_transient: self.inner.config.retry_transient,
                max_compaction_handoffs: self.inner.config.max_compaction_handoffs,
                connection_status: observation.connection.as_str().to_owned(),
                model_calls: observation.logical_calls,
                model_attempts: observation.attempts,
                retries_scheduled: observation.retries_scheduled,
                persistence: "memory_json_sessions_in_memory".to_owned(),
            },
            sessions,
            memory: memory_snapshot(self.inner.memory.store.snapshot().await),
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<EventNotification> {
        self.inner.events.subscribe()
    }

    pub(crate) async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionSnapshot, RuntimeError> {
        let sequence = self.inner.next_session.fetch_add(1, Ordering::AcqRel) + 1;
        let id = DemoSessionId(format!("session-{sequence}"));
        let title = normalize_title(request.title, sequence)?;
        let temporary_directory = tempfile::Builder::new()
            .prefix(&format!("{id}-"))
            .tempdir_in(&self.inner.config.data_dir)
            .map_err(|_| RuntimeError::Workspace)?;
        let temporary_workspace = AbsolutePath::new(temporary_directory.path().to_path_buf())
            .map_err(|_| RuntimeError::Path)?;
        let pinned_store = self.inner.memory.store.snapshot().await;
        let pinned_snapshot = PinnedMemorySnapshot::render(
            PinnedMemorySnapshotInput {
                description: "These durable entries were frozen when this session was created. Memory tool changes affect future new sessions, not this snapshot.".to_owned(),
                entries: pinned_store.entries.clone(),
            },
            &self.inner.memory.limits,
        )
        .map_err(|error| RuntimeError::Memory(error.to_string()))?;
        let frozen_system_prompt = SystemPromptSnapshot::new(vec![
            "You are the ez-assistant Core Demo agent. Use dedicated tools when they materially help complete the user's request. File and Shell authorization is enforced by the host.".to_owned(),
            pinned_snapshot.into_content(),
            format!(
                "recall_memory searches larger information on demand. `{DEMO_SOURCE_ID}` is the default local record source. `{FAILING_SOURCE_ID}` intentionally fails so partial-source behavior can be inspected. Recall results are temporary and are never pinned automatically."
            ),
        ]);
        let frozen_prompt_summary = FrozenPromptSummary {
            part_count: frozen_system_prompt.parts().len(),
            pinned_revision: pinned_store.revision,
            pinned_entry_count: pinned_store.entries.len(),
            recall_sources: vec![DEMO_SOURCE_ID.to_owned(), FAILING_SOURCE_ID.to_owned()],
        };
        let agent = build_agent(
            self.inner.model.clone(),
            frozen_system_prompt.clone(),
            self.inner.context_window.clone(),
            self.inner.tools.clone(),
            self.inner.model_request.clone(),
        )?;
        let session = Arc::new(DemoSession {
            id: id.clone(),
            title,
            agent,
            frozen_system_prompt,
            frozen_prompt_summary,
            journal: DemoJournal::default(),
            audit: DemoAudit::default(),
            approvals: ApprovalCoordinator::default(),
            _temporary_directory: temporary_directory,
            temporary_workspace,
            state: Mutex::new(DemoSessionState {
                sequence: 1,
                next_run: 0,
                next_message: 0,
                active_run: None,
                run: None,
            }),
            run_finished: Notify::new(),
            approval_changed: Notify::new(),
        });
        self.sessions_mut().insert(id.clone(), session.clone());
        self.inner.events.publish(&id, 1, EventKind::SessionCreated);
        Ok(session.snapshot())
    }

    /// 直接管理最新 Store；与模型工具共享同一个能力实例。
    pub(crate) async fn pin_memory(
        &self,
        request: PinMemoryRequest,
    ) -> Result<MemoryStoreSnapshot, RuntimeError> {
        let category = PinnedMemoryCategory::new(request.category)
            .map_err(|error| RuntimeError::MemoryInput(error.to_string()))?;
        self.inner
            .memory
            .store
            .pin(
                PinnedMemoryDraft {
                    category,
                    content: request.content,
                    attributes: request.attributes,
                },
                CancellationToken::new(),
            )
            .await
            .map_err(map_store_operation_error)?;
        Ok(memory_snapshot(self.inner.memory.store.snapshot().await))
    }

    pub(crate) async fn update_memory(
        &self,
        id: &str,
        request: UpdateMemoryRequest,
    ) -> Result<MemoryStoreSnapshot, RuntimeError> {
        let id = PinnedMemoryId::new(id)
            .map_err(|error| RuntimeError::MemoryInput(error.to_string()))?;
        let category = request
            .category
            .map(PinnedMemoryCategory::new)
            .transpose()
            .map_err(|error| RuntimeError::MemoryInput(error.to_string()))?;
        self.inner
            .memory
            .store
            .update(
                id,
                PinnedMemoryPatch {
                    category,
                    content: request.content,
                    attributes: request.attributes,
                },
                CancellationToken::new(),
            )
            .await
            .map_err(map_store_operation_error)?;
        Ok(memory_snapshot(self.inner.memory.store.snapshot().await))
    }

    pub(crate) async fn unpin_memory(&self, id: &str) -> Result<MemoryStoreSnapshot, RuntimeError> {
        let id = PinnedMemoryId::new(id)
            .map_err(|error| RuntimeError::MemoryInput(error.to_string()))?;
        self.inner
            .memory
            .store
            .unpin(id, CancellationToken::new())
            .await
            .map_err(map_store_operation_error)?;
        Ok(memory_snapshot(self.inner.memory.store.snapshot().await))
    }

    pub(crate) fn session_snapshot(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshot, RuntimeError> {
        Ok(self.session(session_id)?.snapshot())
    }

    pub(crate) fn start_run(
        &self,
        session_id: &str,
        request: StartRunRequest,
    ) -> Result<SessionSnapshot, RuntimeError> {
        let message = request.message.trim();
        if message.is_empty() {
            return Err(RuntimeError::EmptyMessage);
        }
        if message.chars().count() > MAX_MESSAGE_CHARS {
            return Err(RuntimeError::MessageTooLong);
        }

        let session = self.session(session_id)?;
        let (run_id, cancellation, conversation, session_sequence) = {
            let mut state = session.lock_state();
            if state.active_run.is_some() || session.journal.has_pending() {
                return Err(RuntimeError::SessionBusy);
            }
            state.next_run = state.next_run.saturating_add(1);
            state.next_message = state.next_message.saturating_add(1);
            let run_id = DemoRunId(format!("{}-run-{}", session.id, state.next_run));
            let user_message = UserMessage {
                id: MessageId::new(format!("{}-user-{}", session.id, state.next_message))
                    .map_err(|_| RuntimeError::Identifier)?,
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new(format!("{}-user-text-{}", session.id, state.next_message))
                        .map_err(|_| RuntimeError::Identifier)?,
                    text: message.to_owned(),
                })],
            };
            session.journal.append_user(user_message);
            let conversation = session.journal.snapshot();
            let cancellation = CancellationToken::new();
            state.active_run = Some(ActiveRun {
                id: run_id.clone(),
                cancellation: cancellation.clone(),
            });
            state.run = Some(RunSnapshot {
                run_id: run_id.to_string(),
                status: RunStatus::Running,
                execution_mode: request.execution_mode,
                approval_mode: request.approval_mode,
                cancel_requested: false,
                event_count: 0,
                dropped_events: 0,
                guardrail_triggers: 0,
                compaction_handoffs: 0,
                reasoning: String::new(),
                text: String::new(),
                last_event: None,
                last_error: None,
                tools: Vec::new(),
            });
            state.sequence = state.sequence.saturating_add(1);
            (run_id, cancellation, conversation, state.sequence)
        };

        self.inner.events.publish(
            &session.id,
            session_sequence,
            EventKind::RunStarted {
                run_id: run_id.to_string(),
            },
        );

        let approval_authorizer: Arc<dyn ToolAuthorizer> = Arc::new(DemoApprovalAuthorizer::new(
            run_id.to_string(),
            session.approvals.clone(),
            cancellation.clone(),
            session.audit.clone(),
            self.approval_notifier(session.clone(), run_id.clone()),
        ));
        let authorizer = match request.execution_mode {
            ExecutionMode::Plan => plan_authorizer(
                &run_id.to_string(),
                self.inner.workdir.clone(),
                session.temporary_workspace.clone(),
                session.audit.clone(),
            ),
            ExecutionMode::Build => build_authorizer(
                &run_id.to_string(),
                self.inner.workdir.clone(),
                session.temporary_workspace.clone(),
                self.inner.denied_workspace.clone(),
                request.approval_mode,
                approval_authorizer,
                session.audit.clone(),
            ),
        };
        let execution = start_execution(
            &session,
            &run_id,
            conversation,
            cancellation.clone(),
            authorizer.clone(),
        );
        let supervisor_runtime = self.clone();
        let supervisor_session = session.clone();
        tokio::spawn(async move {
            let panic_session = supervisor_session.clone();
            let panic_run_id = run_id.clone();
            let panic_runtime = supervisor_runtime.clone();
            let result = AssertUnwindSafe(supervise(
                supervisor_session,
                run_id,
                execution,
                cancellation,
                authorizer,
                supervisor_runtime,
            ))
            .catch_unwind()
            .await;
            if result.is_err() {
                finish_run_with_error(
                    &panic_session,
                    &panic_run_id,
                    "run supervisor panicked".to_owned(),
                    &panic_runtime,
                );
            }
        });
        Ok(session.snapshot())
    }

    pub(crate) fn cancel_run(&self, session_id: &str) -> Result<SessionSnapshot, RuntimeError> {
        let session = self.session(session_id)?;
        let (run_id, session_sequence) = {
            let mut state = session.lock_state();
            let Some(active) = &state.active_run else {
                return Err(RuntimeError::NoActiveRun);
            };
            let run_id = active.id.clone();
            active.cancellation.cancel();
            if let Some(run) = &mut state.run {
                run.cancel_requested = true;
            }
            state.sequence = state.sequence.saturating_add(1);
            (run_id, state.sequence)
        };
        self.inner.events.publish(
            &session.id,
            session_sequence,
            EventKind::RunCancelRequested {
                run_id: run_id.to_string(),
            },
        );
        Ok(session.snapshot())
    }

    pub(crate) fn decide_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<SessionSnapshot, RuntimeError> {
        let session = self.session(session_id)?;
        session
            .approvals
            .decide(approval_id, decision)
            .map_err(RuntimeError::Approval)?;
        self.notify_approval_change(&session, None);
        Ok(session.snapshot())
    }

    fn approval_notifier(
        &self,
        session: Arc<DemoSession>,
        run_id: DemoRunId,
    ) -> StateChangeNotifier {
        let runtime = self.clone();
        Arc::new(move || runtime.notify_approval_change(&session, Some(&run_id)))
    }

    fn notify_approval_change(&self, session: &DemoSession, expected_run: Option<&DemoRunId>) {
        let (run_id, sequence) = {
            let mut state = session.lock_state();
            let Some(active) = &state.active_run else {
                return;
            };
            if expected_run.is_some_and(|expected| expected != &active.id) {
                return;
            }
            let run_id = active.id.to_string();
            state.sequence = state.sequence.saturating_add(1);
            (run_id, state.sequence)
        };
        self.inner
            .events
            .publish(&session.id, sequence, EventKind::ApprovalChanged { run_id });
        session.approval_changed.notify_waiters();
    }

    fn session(&self, session_id: &str) -> Result<Arc<DemoSession>, RuntimeError> {
        self.sessions()
            .get(&DemoSessionId(session_id.to_owned()))
            .cloned()
            .ok_or(RuntimeError::SessionNotFound)
    }

    fn sessions(&self) -> RwLockReadGuard<'_, BTreeMap<DemoSessionId, Arc<DemoSession>>> {
        self.inner
            .sessions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn sessions_mut(&self) -> RwLockWriteGuard<'_, BTreeMap<DemoSessionId, Arc<DemoSession>>> {
        self.inner
            .sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_idle(&self, session_id: &str) {
        let session = self.session(session_id).expect("test session exists");
        loop {
            let notified = session.run_finished.notified();
            if session.lock_state().active_run.is_none() {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_approval(
        &self,
        session_id: &str,
    ) -> crate::approval::PendingApprovalSnapshot {
        let session = self.session(session_id).expect("test session exists");
        loop {
            let notified = session.approval_changed.notified();
            if let Some(approval) = session.approvals.snapshot() {
                return approval;
            }
            notified.await;
        }
    }
}

impl DemoSession {
    fn summary(&self) -> SessionSummary {
        let state = self.lock_state();
        SessionSummary {
            session_id: self.id.to_string(),
            title: self.title.clone(),
            active_run: state.active_run.is_some(),
            last_status: state.run.as_ref().map(|run| run.status),
            sequence: state.sequence,
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        let state = self.lock_state();
        let conversation = self.journal.snapshot();
        let mut frozen_prompt = self.frozen_prompt_summary.clone();
        frozen_prompt.part_count = self.frozen_system_prompt.parts().len();
        SessionSnapshot {
            session_id: self.id.to_string(),
            title: self.title.clone(),
            sequence: state.sequence,
            active_run: state.active_run.is_some(),
            pending_exchange: self.journal.has_pending(),
            temporary_workspace: self.temporary_workspace.to_string(),
            frozen_prompt,
            journal: conversation.messages,
            run: state.run.clone(),
            approval: self.approvals.snapshot(),
            audit: self.audit.entries(),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, DemoSessionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn build_agent(
    model: Arc<dyn ModelService>,
    system_prompt: SystemPromptSnapshot,
    context_window: Arc<ContextWindowEvaluator>,
    tools: ToolSetSnapshot,
    model_request: ModelRequestConfig,
) -> Result<Agent, RuntimeError> {
    AgentBuilder::new(model, system_prompt, context_window)
        .tools(tools)
        .model_request(model_request)
        .guardrails(GuardrailConfig {
            repeated_invocation: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Observe,
                threshold: std::num::NonZeroU32::new(3).expect("three is non-zero"),
            }),
            consecutive_failures: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Enforce,
                threshold: std::num::NonZeroU32::new(5).expect("five is non-zero"),
            }),
        })
        .build()
        .map_err(|_| RuntimeError::AgentBuild)
}

fn memory_snapshot(snapshot: DemoPinnedStoreSnapshot) -> MemoryStoreSnapshot {
    MemoryStoreSnapshot {
        revision: snapshot.revision,
        entries: snapshot.entries,
    }
}

fn map_store_operation_error(error: PinnedMemoryStoreError) -> RuntimeError {
    match error {
        PinnedMemoryStoreError::InvalidInput { .. }
        | PinnedMemoryStoreError::NotFound { .. }
        | PinnedMemoryStoreError::CapacityExceeded { .. } => {
            RuntimeError::MemoryInput(error.to_string())
        }
        PinnedMemoryStoreError::Corrupt { .. }
        | PinnedMemoryStoreError::Io { .. }
        | PinnedMemoryStoreError::Cancelled => RuntimeError::Memory(error.to_string()),
    }
}

fn start_execution(
    session: &DemoSession,
    run_id: &DemoRunId,
    conversation: agent_types::ConversationSnapshot,
    cancellation: CancellationToken,
    authorizer: Arc<dyn ToolAuthorizer>,
) -> agent_sdk::AgentExecution {
    session.agent.start(
        ExecutionInput { conversation },
        ExecutionContext {
            cancellation,
            recorder: DemoRecorder::new(
                session.journal.clone(),
                run_id.to_string(),
                session.audit.clone(),
            ),
            authorizer,
        },
    )
}

async fn supervise(
    session: Arc<DemoSession>,
    run_id: DemoRunId,
    mut execution: agent_sdk::AgentExecution,
    cancellation: CancellationToken,
    authorizer: Arc<dyn ToolAuthorizer>,
    runtime: DemoRuntime,
) {
    loop {
        while let Some(event) = execution.events.next().await {
            record_event(&session, &run_id, &event, &runtime);
        }
        let outcome = execution.completion.await;
        let ExecutionOutcome::CompactionRequired { reason, step, .. } = outcome else {
            finish_run(&session, &run_id, outcome, &runtime);
            return;
        };

        if cancellation.is_cancelled() {
            finish_run(&session, &run_id, ExecutionOutcome::Cancelled, &runtime);
            return;
        }
        if !reserve_compaction_handoff(&session, &run_id, reason, step, &runtime) {
            finish_run_with_error(
                &session,
                &run_id,
                format!(
                    "context compaction handoff limit ({}) reached at step {step}",
                    runtime.inner.config.max_compaction_handoffs
                ),
                &runtime,
            );
            return;
        }

        let checkpoint = session.journal.snapshot();
        let replacement = runtime
            .inner
            .compaction
            .compact(
                runtime.inner.model.clone(),
                session.frozen_system_prompt.clone(),
                checkpoint,
                cancellation.clone(),
            )
            .await;
        let replacement = match replacement {
            Ok(replacement) => replacement,
            Err(_error) if cancellation.is_cancelled() => {
                finish_run(&session, &run_id, ExecutionOutcome::Cancelled, &runtime);
                return;
            }
            Err(error) => {
                finish_run_with_error(&session, &run_id, error.to_string(), &runtime);
                return;
            }
        };
        if let Err(error) = session.journal.replace_snapshot(replacement) {
            finish_run_with_error(&session, &run_id, error.to_string(), &runtime);
            return;
        }

        let conversation = session.journal.snapshot();
        execution = start_execution(
            &session,
            &run_id,
            conversation,
            cancellation.clone(),
            authorizer.clone(),
        );
    }
}

fn reserve_compaction_handoff(
    session: &DemoSession,
    run_id: &DemoRunId,
    reason: agent_core::CompactionReason,
    step: u32,
    runtime: &DemoRuntime,
) -> bool {
    let sequence = {
        let mut state = session.lock_state();
        if state
            .active_run
            .as_ref()
            .is_none_or(|active| active.id != *run_id)
        {
            return false;
        }
        let Some(run) = &mut state.run else {
            return false;
        };
        if run.compaction_handoffs >= runtime.inner.config.max_compaction_handoffs {
            return false;
        }
        run.compaction_handoffs += 1;
        run.reasoning.clear();
        run.text.clear();
        run.last_event = Some(format!(
            "compaction_handoff_{}_{reason:?}_step_{step}",
            run.compaction_handoffs
        ));
        state.sequence = state.sequence.saturating_add(1);
        state.sequence
    };
    runtime.inner.events.publish(
        &session.id,
        sequence,
        EventKind::RunProgress {
            run_id: run_id.to_string(),
            event: "compaction_handoff".to_owned(),
        },
    );
    true
}

fn record_event(
    session: &DemoSession,
    run_id: &DemoRunId,
    event: &AgentEvent,
    runtime: &DemoRuntime,
) {
    let event_name = event_name(event).to_owned();
    let session_sequence = {
        let mut state = session.lock_state();
        if state
            .active_run
            .as_ref()
            .is_none_or(|active| active.id != *run_id)
        {
            return;
        }
        if let Some(run) = &mut state.run {
            run.event_count = run.event_count.saturating_add(1);
            run.last_event = Some(event_name.clone());
            match event {
                AgentEvent::ReasoningDelta { delta, .. } => run.reasoning.push_str(delta),
                AgentEvent::TextDelta { delta, .. } => run.text.push_str(delta),
                AgentEvent::ToolProposed { call } => run.tools.push(ToolActivitySnapshot {
                    call_id: call.id.to_string(),
                    tool_name: call.name.as_str().to_owned(),
                    status: "proposed".to_owned(),
                    stdout: String::new(),
                    stderr: String::new(),
                }),
                AgentEvent::ToolStarted { call_id } => {
                    session.audit.record_started(&run_id.to_string(), call_id);
                    if let Some(tool) = run
                        .tools
                        .iter_mut()
                        .find(|tool| tool.call_id == call_id.to_string())
                    {
                        tool.status = "started".to_owned();
                    }
                }
                AgentEvent::ToolOutput {
                    call_id,
                    channel,
                    chunk,
                } => {
                    if let Some(tool) = run
                        .tools
                        .iter_mut()
                        .find(|tool| tool.call_id == call_id.to_string())
                    {
                        match channel {
                            ToolOutputChannel::Stdout => tool.stdout.push_str(chunk),
                            ToolOutputChannel::Stderr => tool.stderr.push_str(chunk),
                        }
                    }
                }
                AgentEvent::ToolCompleted { call_id, status } => {
                    if let Some(tool) = run
                        .tools
                        .iter_mut()
                        .find(|tool| tool.call_id == call_id.to_string())
                    {
                        tool.status = format!("{status:?}").to_ascii_lowercase();
                    }
                }
                AgentEvent::GuardrailTriggered { .. } => {
                    run.guardrail_triggers = run.guardrail_triggers.saturating_add(1);
                }
                AgentEvent::ExecutionCompleted { dropped_events, .. }
                | AgentEvent::ExecutionFailed { dropped_events, .. }
                | AgentEvent::ExecutionCancelled { dropped_events }
                | AgentEvent::ExecutionCompactionRequired { dropped_events, .. } => {
                    run.dropped_events = *dropped_events;
                }
                _ => {}
            }
        }
        state.sequence = state.sequence.saturating_add(1);
        state.sequence
    };
    runtime.inner.events.publish(
        &session.id,
        session_sequence,
        EventKind::RunProgress {
            run_id: run_id.to_string(),
            event: event_name,
        },
    );
}

fn finish_run(
    session: &DemoSession,
    run_id: &DemoRunId,
    outcome: ExecutionOutcome,
    runtime: &DemoRuntime,
) {
    let (status, session_sequence) = {
        let mut state = session.lock_state();
        if state
            .active_run
            .as_ref()
            .is_none_or(|active| active.id != *run_id)
        {
            return;
        }
        let status = match outcome {
            ExecutionOutcome::Completed(message) => {
                if let Some(run) = &mut state.run {
                    let (reasoning, text) = completed_text(&message);
                    run.reasoning = reasoning;
                    run.text = text;
                }
                session.journal.append_assistant(message);
                RunStatus::Completed
            }
            ExecutionOutcome::Failed(error) => {
                if let Some(run) = &mut state.run {
                    run.last_error = Some(error.to_string());
                }
                RunStatus::Failed
            }
            ExecutionOutcome::Cancelled => RunStatus::Cancelled,
            ExecutionOutcome::CompactionRequired { reason, step, .. } => {
                if let Some(run) = &mut state.run {
                    run.last_error = Some(format!(
                        "context compaction required at step {step}: {reason:?}"
                    ));
                }
                RunStatus::CompactionRequired
            }
        };
        if let Some(run) = &mut state.run {
            run.status = status;
        }
        state.active_run = None;
        state.sequence = state.sequence.saturating_add(1);
        (status, state.sequence)
    };
    runtime.inner.events.publish(
        &session.id,
        session_sequence,
        EventKind::RunFinished {
            run_id: run_id.to_string(),
            status,
        },
    );
    session.run_finished.notify_waiters();
}

fn finish_run_with_error(
    session: &DemoSession,
    run_id: &DemoRunId,
    error: String,
    runtime: &DemoRuntime,
) {
    let session_sequence = {
        let mut state = session.lock_state();
        if state
            .active_run
            .as_ref()
            .is_none_or(|active| active.id != *run_id)
        {
            return;
        }
        if let Some(run) = &mut state.run {
            run.status = RunStatus::Failed;
            run.last_error = Some(error);
        }
        state.active_run = None;
        state.sequence = state.sequence.saturating_add(1);
        state.sequence
    };
    runtime.inner.events.publish(
        &session.id,
        session_sequence,
        EventKind::RunFinished {
            run_id: run_id.to_string(),
            status: RunStatus::Failed,
        },
    );
    session.run_finished.notify_waiters();
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

fn completed_text(message: &AssistantMessage) -> (String, String) {
    let mut reasoning = String::new();
    let mut text = String::new();
    for part in &message.parts {
        match part {
            AssistantPart::Reasoning(part) => reasoning.push_str(&part.text),
            AssistantPart::Text(part) => text.push_str(&part.text),
            AssistantPart::ToolCall(_) | AssistantPart::ProviderState(_) => {}
        }
    }
    (reasoning, text)
}

fn normalize_title(title: Option<String>, sequence: u64) -> Result<String, RuntimeError> {
    let title = title.unwrap_or_else(|| format!("Session {sequence}"));
    let title = title.trim();
    if title.is_empty() {
        return Err(RuntimeError::EmptyTitle);
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(RuntimeError::TitleTooLong);
    }
    Ok(title.to_owned())
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum RuntimeError {
    #[error("context window configuration is invalid")]
    InvalidContextWindow,
    #[error("model resources could not be configured: {0}")]
    ModelResources(String),
    #[error("agent could not be built")]
    AgentBuild,
    #[error("local tools could not be configured")]
    Tooling,
    #[error("memory resources could not be configured: {0}")]
    Memory(String),
    #[error("memory request is invalid: {0}")]
    MemoryInput(String),
    #[error("session workspace could not be created")]
    Workspace,
    #[error("demo path could not be resolved")]
    Path,
    #[error("session was not found")]
    SessionNotFound,
    #[error("session title must not be empty")]
    EmptyTitle,
    #[error("session title is too long")]
    TitleTooLong,
    #[error("message must not be empty")]
    EmptyMessage,
    #[error("message is too long")]
    MessageTooLong,
    #[error("session already has an active run")]
    SessionBusy,
    #[error("there is no active run")]
    NoActiveRun,
    #[error("approval decision failed: {0}")]
    Approval(ApprovalError),
    #[error("generated identifier is invalid")]
    Identifier,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
        time::Duration,
    };

    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelEvent, ModelEventStream, ModelRequest,
        ModelStreamFuture,
    };
    use agent_types::{FinishReason, ModelIdentity, ProviderId};
    use futures_util::stream;
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        cli::ServeArguments,
        config::ServeConfig,
        wire::{ApprovalMode, ExecutionMode, PinMemoryRequest},
    };

    struct TwoCallBarrierModel {
        capabilities: ModelCapabilities,
        arrivals: AtomicUsize,
        next_message: AtomicU64,
        release: Notify,
    }

    impl TwoCallBarrierModel {
        fn new() -> Self {
            Self {
                capabilities: ModelCapabilities {
                    reasoning: true,
                    image_input: false,
                    tool_calls: true,
                    multimodal_tool_result: false,
                    tool_choice: agent_model::ToolChoiceCapabilities::all(),
                    streaming: true,
                },
                arrivals: AtomicUsize::new(0),
                next_message: AtomicU64::new(0),
                release: Notify::new(),
            }
        }
    }

    impl ModelService for TwoCallBarrierModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            4_096
        }

        fn stream(
            &self,
            _request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(async move {
                let arrived = self.arrivals.fetch_add(1, Ordering::AcqRel) + 1;
                if arrived >= 2 {
                    self.release.notify_waiters();
                }
                while self.arrivals.load(Ordering::Acquire) < 2 {
                    tokio::select! {
                        () = context.cancellation.cancelled() => {
                            return Err(agent_model::ModelError::Cancelled);
                        }
                        () = self.release.notified() => {}
                    }
                }
                let sequence = self.next_message.fetch_add(1, Ordering::AcqRel) + 1;
                let message_id = MessageId::new(format!("barrier-message-{sequence}"))
                    .expect("valid message id");
                let identity = ModelIdentity::new(
                    ProviderId::new("barrier").expect("valid provider"),
                    "barrier-model",
                );
                let message = AssistantMessage {
                    id: message_id.clone(),
                    model: identity.clone(),
                    parts: vec![AssistantPart::Text(TextPart {
                        id: PartId::new(format!("barrier-text-{sequence}")).expect("valid part id"),
                        text: "completed concurrently".to_owned(),
                    })],
                    finish_reason: FinishReason::Stop,
                    usage: None,
                };
                Ok(Box::pin(stream::iter([
                    ModelEvent::TurnStarted {
                        message_id,
                        model: identity,
                    },
                    ModelEvent::TurnFinished { message },
                ])) as ModelEventStream)
            })
        }
    }

    struct PanicModel {
        capabilities: ModelCapabilities,
    }

    impl PanicModel {
        fn new() -> Self {
            Self {
                capabilities: ModelCapabilities {
                    reasoning: true,
                    image_input: false,
                    tool_calls: true,
                    multimodal_tool_result: false,
                    tool_choice: agent_model::ToolChoiceCapabilities::all(),
                    streaming: true,
                },
            }
        }
    }

    impl ModelService for PanicModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            4_096
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            panic!("intentional model panic for supervisor recovery test")
        }
    }

    async fn runtime() -> (DemoRuntime, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("create temp root");
        let config = ServeConfig::resolve(ServeArguments {
            workdir: root.path().to_path_buf(),
            data_dir: root.path().join("data"),
            port: 0,
            max_compaction_handoffs: crate::cli::DEFAULT_MAX_COMPACTION_HANDOFFS,
            retry_transient: false,
        })
        .expect("resolve config");
        (
            DemoRuntime::new_offline(config)
                .await
                .expect("create runtime"),
            root,
        )
    }

    async fn compacting_runtime(max_compaction_handoffs: u32) -> (DemoRuntime, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("create temp root");
        let config = ServeConfig::resolve(ServeArguments {
            workdir: root.path().to_path_buf(),
            data_dir: root.path().join("data"),
            port: 0,
            max_compaction_handoffs,
            retry_transient: false,
        })
        .expect("resolve config");
        let model: Arc<dyn ModelService> = Arc::new(
            crate::model::DeterministicModel::with_context_window(Duration::ZERO, 120),
        );
        (
            DemoRuntime::new_offline_with_model(config, model)
                .await
                .expect("create compacting runtime"),
            root,
        )
    }

    async fn runtime_with_model(model: Arc<dyn ModelService>) -> (DemoRuntime, tempfile::TempDir) {
        let root = tempfile::tempdir().expect("create temp root");
        let config = ServeConfig::resolve(ServeArguments {
            workdir: root.path().to_path_buf(),
            data_dir: root.path().join("data"),
            port: 0,
            max_compaction_handoffs: crate::cli::DEFAULT_MAX_COMPACTION_HANDOFFS,
            retry_transient: false,
        })
        .expect("resolve config");
        (
            DemoRuntime::new_offline_with_model(config, model)
                .await
                .expect("create runtime with model"),
            root,
        )
    }

    fn start_request(message: &str) -> StartRunRequest {
        StartRunRequest {
            message: message.to_owned(),
            execution_mode: ExecutionMode::Build,
            approval_mode: ApprovalMode::Auto,
        }
    }

    fn mode_request(
        message: &str,
        execution_mode: ExecutionMode,
        approval_mode: ApprovalMode,
    ) -> StartRunRequest {
        StartRunRequest {
            message: message.to_owned(),
            execution_mode,
            approval_mode,
        }
    }

    async fn run_to_idle(
        runtime: &DemoRuntime,
        session_id: &str,
        request: StartRunRequest,
    ) -> SessionSnapshot {
        runtime.start_run(session_id, request).expect("start run");
        tokio::time::timeout(Duration::from_secs(5), runtime.wait_until_idle(session_id))
            .await
            .expect("run completes");
        runtime
            .session_snapshot(session_id)
            .expect("final snapshot")
    }

    #[tokio::test]
    async fn same_session_is_busy_until_completion() {
        let (runtime, _root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");
        runtime
            .start_run(&session.session_id, start_request("first"))
            .expect("start first run");
        assert_eq!(
            runtime.start_run(&session.session_id, start_request("second")),
            Err(RuntimeError::SessionBusy)
        );

        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_idle(&session.session_id),
        )
        .await
        .expect("first run completes");
        runtime
            .start_run(&session.session_id, start_request("second"))
            .expect("start second after completion");
    }

    #[tokio::test]
    async fn core_task_panic_fails_run_and_releases_session_gate() {
        let model: Arc<dyn ModelService> = Arc::new(PanicModel::new());
        let (runtime, _root) = runtime_with_model(model).await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");

        runtime
            .start_run(&session.session_id, start_request("panic"))
            .expect("start run");
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_idle(&session.session_id),
        )
        .await
        .expect("panic is converted to a terminal run");

        let snapshot = runtime
            .session_snapshot(&session.session_id)
            .expect("session snapshot");
        let run = snapshot.run.expect("run snapshot");
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(
            run.last_error.as_deref(),
            Some("agent execution task terminated unexpectedly")
        );
        assert!(
            !run.last_error
                .as_deref()
                .is_some_and(|message| message.contains("intentional model panic"))
        );
        assert!(
            !runtime
                .session(&session.session_id)
                .expect("session")
                .summary()
                .active_run
        );
    }

    #[tokio::test]
    async fn cancelling_one_session_does_not_cancel_another() {
        let (runtime, _root) = runtime().await;
        let first = runtime
            .create_session(CreateSessionRequest {
                title: Some("First".to_owned()),
            })
            .await
            .expect("create first session");
        let second = runtime
            .create_session(CreateSessionRequest {
                title: Some("Second".to_owned()),
            })
            .await
            .expect("create second session");
        runtime
            .start_run(&first.session_id, start_request("cancel me"))
            .expect("start first");
        runtime
            .start_run(&second.session_id, start_request("finish me"))
            .expect("start second");
        runtime.cancel_run(&first.session_id).expect("cancel first");

        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                runtime.wait_until_idle(&first.session_id),
                runtime.wait_until_idle(&second.session_id)
            );
        })
        .await
        .expect("both runs settle");

        assert_eq!(
            runtime
                .session_snapshot(&first.session_id)
                .expect("first snapshot")
                .run
                .expect("first run")
                .status,
            RunStatus::Cancelled
        );
        assert_eq!(
            runtime
                .session_snapshot(&second.session_id)
                .expect("second snapshot")
                .run
                .expect("second run")
                .status,
            RunStatus::Completed
        );
    }

    #[tokio::test]
    async fn two_sessions_reach_the_same_shared_model_concurrently() {
        let model = Arc::new(TwoCallBarrierModel::new());
        let (runtime, _root) = runtime_with_model(model.clone()).await;
        let first = runtime
            .create_session(CreateSessionRequest {
                title: Some("First".to_owned()),
            })
            .await
            .expect("create first");
        let second = runtime
            .create_session(CreateSessionRequest {
                title: Some("Second".to_owned()),
            })
            .await
            .expect("create second");
        runtime
            .start_run(&first.session_id, start_request("first"))
            .expect("start first");
        runtime
            .start_run(&second.session_id, start_request("second"))
            .expect("start second");

        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                runtime.wait_until_idle(&first.session_id),
                runtime.wait_until_idle(&second.session_id)
            );
        })
        .await
        .expect("both sessions pass the two-call barrier");

        assert_eq!(model.arrivals.load(Ordering::Acquire), 2);
        assert_eq!(
            runtime
                .session_snapshot(&first.session_id)
                .expect("first snapshot")
                .run
                .expect("first run")
                .status,
            RunStatus::Completed
        );
        assert_eq!(
            runtime
                .session_snapshot(&second.session_id)
                .expect("second snapshot")
                .run
                .expect("second run")
                .status,
            RunStatus::Completed
        );
    }

    #[tokio::test]
    async fn lagged_or_disconnected_observer_does_not_change_completion() {
        let (runtime, _root) = runtime().await;
        let mut lagged = runtime.subscribe();
        let disconnected = runtime.subscribe();
        drop(disconnected);
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");
        runtime
            .start_run(&session.session_id, start_request("produce many events"))
            .expect("start run");
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_idle(&session.session_id),
        )
        .await
        .expect("run completes");

        assert!(matches!(
            lagged.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        let snapshot = runtime
            .session_snapshot(&session.session_id)
            .expect("session snapshot");
        assert_eq!(snapshot.run.expect("run").status, RunStatus::Completed);
        assert_eq!(snapshot.journal.len(), 2);
    }

    #[tokio::test]
    async fn compaction_replaces_checkpoint_and_continues_with_frozen_agent() {
        let (runtime, _root) = compacting_runtime(2).await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");
        let prompt_before = runtime
            .session(&session.session_id)
            .expect("session")
            .agent
            .system_prompt()
            .clone();
        let large_turn = "a".repeat(180);
        let first = run_to_idle(&runtime, &session.session_id, start_request(&large_turn)).await;
        assert_eq!(first.run.expect("first run").status, RunStatus::Completed);

        let second = run_to_idle(
            &runtime,
            &session.session_id,
            start_request("continue after compaction"),
        )
        .await;
        let run = second.run.expect("second run");
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.compaction_handoffs, 1);
        assert!(matches!(
            second.journal.first(),
            Some(agent_types::ConversationMessage::ContextSummary(_))
        ));
        assert_eq!(
            runtime
                .session(&session.session_id)
                .expect("session")
                .agent
                .system_prompt(),
            &prompt_before
        );
    }

    #[tokio::test]
    async fn compaction_handoff_limit_is_a_diagnostic_terminal_failure() {
        let (runtime, _root) = compacting_runtime(0).await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");
        run_to_idle(
            &runtime,
            &session.session_id,
            start_request(&"b".repeat(180)),
        )
        .await;
        let snapshot = run_to_idle(
            &runtime,
            &session.session_id,
            start_request("cannot hand off"),
        )
        .await;
        let run = snapshot.run.expect("limited run");
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.compaction_handoffs, 0);
        assert!(
            run.last_error
                .expect("diagnostic error")
                .contains("handoff limit (0)")
        );
    }

    #[tokio::test]
    async fn plan_denies_workdir_write_in_both_approval_modes() {
        for approval_mode in [ApprovalMode::Ask, ApprovalMode::Auto] {
            let (runtime, root) = runtime().await;
            let session = runtime
                .create_session(CreateSessionRequest::default())
                .await
                .expect("session");
            let snapshot = run_to_idle(
                &runtime,
                &session.session_id,
                mode_request(
                    r#"/tool write_file {"path":"plan.txt","content":"blocked"}"#,
                    ExecutionMode::Plan,
                    approval_mode,
                ),
            )
            .await;
            assert!(!root.path().join("plan.txt").exists());
            assert!(snapshot.approval.is_none());
            assert_eq!(snapshot.run.expect("run").status, RunStatus::Completed);
            assert_eq!(
                snapshot.audit.last().expect("audit").status,
                crate::audit::AuditExecutionStatus::Denied
            );
        }
    }

    #[tokio::test]
    async fn build_auto_allows_unmatched_write_and_records_result() {
        let (runtime, root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        let snapshot = run_to_idle(
            &runtime,
            &session.session_id,
            mode_request(
                r#"/tool write_file {"path":"auto.txt","content":"written"}"#,
                ExecutionMode::Build,
                ApprovalMode::Auto,
            ),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(root.path().join("auto.txt")).expect("file"),
            "written"
        );
        assert!(
            snapshot
                .audit
                .iter()
                .any(|entry| entry.policy == "auto_allow_all"
                    && entry.status == crate::audit::AuditExecutionStatus::Completed)
        );
    }

    #[tokio::test]
    async fn build_ask_can_allow_or_deny_once_and_reject_repeated_response() {
        for decision in [ApprovalDecision::AllowOnce, ApprovalDecision::Deny] {
            let (runtime, root) = runtime().await;
            let session = runtime
                .create_session(CreateSessionRequest::default())
                .await
                .expect("session");
            runtime
                .start_run(
                    &session.session_id,
                    mode_request(
                        r#"/tool write_file {"path":"ask.txt","content":"choice"}"#,
                        ExecutionMode::Build,
                        ApprovalMode::Ask,
                    ),
                )
                .expect("start");
            let approval = tokio::time::timeout(
                Duration::from_secs(5),
                runtime.wait_until_approval(&session.session_id),
            )
            .await
            .expect("approval");
            runtime
                .decide_approval(&session.session_id, &approval.approval_id, decision)
                .expect("decide");
            assert!(matches!(
                runtime.decide_approval(&session.session_id, &approval.approval_id, decision),
                Err(RuntimeError::Approval(
                    ApprovalError::AlreadyDecided | ApprovalError::NotPending
                ))
            ));
            tokio::time::timeout(
                Duration::from_secs(5),
                runtime.wait_until_idle(&session.session_id),
            )
            .await
            .expect("run completes");
            assert_eq!(
                root.path().join("ask.txt").exists(),
                decision == ApprovalDecision::AllowOnce
            );
        }
    }

    #[tokio::test]
    async fn cancelling_pending_approval_has_one_cancelled_terminal_state() {
        let (runtime, _root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        runtime
            .start_run(
                &session.session_id,
                mode_request(
                    r#"/tool shell {"command":"echo should-not-run"}"#,
                    ExecutionMode::Build,
                    ApprovalMode::Ask,
                ),
            )
            .expect("start");
        let approval = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_approval(&session.session_id),
        )
        .await
        .expect("approval");
        runtime.cancel_run(&session.session_id).expect("cancel");
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_idle(&session.session_id),
        )
        .await
        .expect("cancel settles");
        let snapshot = runtime
            .session_snapshot(&session.session_id)
            .expect("snapshot");
        assert_eq!(snapshot.run.expect("run").status, RunStatus::Cancelled);
        assert!(snapshot.approval.is_none());
        assert!(matches!(
            runtime.decide_approval(
                &session.session_id,
                &approval.approval_id,
                ApprovalDecision::AllowOnce
            ),
            Err(RuntimeError::Approval(ApprovalError::NotPending))
        ));
    }

    #[tokio::test]
    async fn cancellation_wins_when_allow_decision_has_not_settled() {
        let (runtime, _root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        runtime
            .start_run(
                &session.session_id,
                mode_request(
                    r#"/tool shell {"command":"echo should-not-run"}"#,
                    ExecutionMode::Build,
                    ApprovalMode::Ask,
                ),
            )
            .expect("start");
        let approval = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_approval(&session.session_id),
        )
        .await
        .expect("approval");
        runtime
            .decide_approval(
                &session.session_id,
                &approval.approval_id,
                ApprovalDecision::AllowOnce,
            )
            .expect("send decision");
        runtime.cancel_run(&session.session_id).expect("cancel run");
        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_until_idle(&session.session_id),
        )
        .await
        .expect("cancel settles");

        let snapshot = runtime
            .session_snapshot(&session.session_id)
            .expect("snapshot");
        assert_eq!(snapshot.run.expect("run").status, RunStatus::Cancelled);
        assert_eq!(
            snapshot.audit.last().expect("audit").status,
            crate::audit::AuditExecutionStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn overlapping_allow_rule_wins_and_guardrail_is_observable() {
        let (runtime, root) = runtime().await;
        let denied = root.path().join(".core-demo-denied");
        std::fs::create_dir(&denied).expect("denied fixture");
        std::fs::write(denied.join("visible.txt"), "visible").expect("fixture");
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        let snapshot = run_to_idle(
            &runtime,
            &session.session_id,
            mode_request(r#"/repeat 3 read_file {"path":".core-demo-denied/visible.txt","offset":1,"limit":10}"#, ExecutionMode::Build, ApprovalMode::Ask),
        ).await;
        assert!(snapshot.approval.is_none());
        assert_eq!(snapshot.run.expect("run").guardrail_triggers, 1);
    }

    #[tokio::test]
    async fn shell_output_stream_is_preserved_in_run_projection() {
        let (runtime, _root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        let snapshot = run_to_idle(
            &runtime,
            &session.session_id,
            mode_request(
                r#"/tool shell {"command":"echo core-demo"}"#,
                ExecutionMode::Build,
                ApprovalMode::Auto,
            ),
        )
        .await;
        let run = snapshot.run.expect("run");
        assert!(run.tools.iter().any(|tool| tool.tool_name == "shell"
            && tool.stdout.contains("core-demo")
            && tool.status == "success"));
    }

    #[tokio::test]
    async fn pinned_store_changes_only_affect_new_session_prompt_snapshots() {
        let (runtime, _root) = runtime().await;
        let first = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("first session");
        let first_session = runtime
            .session(&first.session_id)
            .expect("first session state");
        let frozen_before = first_session.frozen_system_prompt.clone();
        let persisted_prompt =
            serde_json::to_vec(&frozen_before).expect("persist final system prompt fixture");
        assert_eq!(first.frozen_prompt.pinned_revision, 0);

        runtime
            .pin_memory(PinMemoryRequest {
                category: "preference".to_owned(),
                content: "Prefer concise Chinese answers".to_owned(),
                attributes: BTreeMap::new(),
            })
            .await
            .expect("pin memory");

        let same_session = runtime
            .session_snapshot(&first.session_id)
            .expect("same session snapshot");
        assert_eq!(same_session.frozen_prompt.pinned_revision, 0);
        assert_eq!(first_session.frozen_system_prompt, frozen_before);
        assert!(
            !first_session
                .frozen_system_prompt
                .parts()
                .iter()
                .any(|part| part.contains("Prefer concise Chinese answers"))
        );

        let second = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("second session");
        let second_session = runtime
            .session(&second.session_id)
            .expect("second session state");
        assert_eq!(second.frozen_prompt.pinned_revision, 1);
        assert_eq!(second.frozen_prompt.pinned_entry_count, 1);
        assert!(
            second_session
                .frozen_system_prompt
                .parts()
                .iter()
                .any(|part| part.contains("Prefer concise Chinese answers"))
        );

        // 恢复/重建从已持久化的最终 Prompt 反序列化，不重新读取最新 Store。
        let restored_prompt: SystemPromptSnapshot =
            serde_json::from_slice(&persisted_prompt).expect("restore final system prompt");
        let _rebuilt = build_agent(
            runtime.inner.model.clone(),
            restored_prompt.clone(),
            runtime.inner.context_window.clone(),
            runtime.inner.tools.clone(),
            runtime.inner.model_request.clone(),
        )
        .expect("rebuild from frozen prompt");
        assert_eq!(restored_prompt, frozen_before);
        assert_eq!(first_session.frozen_system_prompt, frozen_before);
    }

    #[tokio::test]
    async fn standard_memory_tools_complete_agent_loop_with_partial_recall_failure() {
        let (runtime, _root) = runtime().await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("session");
        let pinned = run_to_idle(
            &runtime,
            &session.session_id,
            start_request(
                r#"/tool pin_memory {"category":"preference","content":"tool-created memory"}"#,
            ),
        )
        .await;
        assert_eq!(pinned.run.expect("pin run").status, RunStatus::Completed);
        assert_eq!(
            runtime.snapshot().await.memory.entries[0].content,
            "tool-created memory"
        );

        let recalled = run_to_idle(
            &runtime,
            &session.session_id,
            start_request(
                r#"/tool recall_memory {"action":"search","query":"memory","limit":4,"sources":["demo_records","failing_demo"]}"#,
            ),
        )
        .await;
        assert_eq!(
            recalled.run.as_ref().expect("recall run").status,
            RunStatus::Completed
        );
        let journal = serde_json::to_string(&recalled.journal).expect("serialize journal");
        assert!(journal.contains(DEMO_SOURCE_ID));
        assert!(journal.contains(FAILING_SOURCE_ID));
        assert!(journal.contains("unavailable"));
        assert!(
            recalled
                .audit
                .iter()
                .any(|entry| entry.tool_name == "recall_memory"
                    && entry.status == crate::audit::AuditExecutionStatus::Completed)
        );
    }
}
