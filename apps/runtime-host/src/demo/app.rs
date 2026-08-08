//! 私有 TUI 的展示状态与纯状态更新；不执行终端绘制或 UDS I/O。

use agent_types::ConversationSnapshot;
use assistant_protocol::{
    ArchiveSessionRequest, CancelRunRequest, ConfigurationState, ConfigurationStatus,
    CreateSessionRequest, GetConfigStatusRequest, GetRunRequest, GetSessionRequest,
    ListModelsRequest, ListRunsRequest, ListSessionsRequest, ModelConfiguration, ModelKey,
    ReloadConfigRequest, RestoreSessionRequest, RunId, RunSnapshot, RunStatus, RuntimeCommand,
    RuntimeCommandResult, RuntimeEvent, SessionId, SessionLifecycle, SessionListFilter,
    SessionSummary, SetSessionModelRequest, ShutdownRuntimeRequest, SubmitInputRequest,
    ToolActivitySnapshot, ToolActivityStatus, ToolCallId, ToolOutputChannel,
    ValidateModelConnectionRequest,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use tui_input::{Input, backend::crossterm::EventHandler};

use crate::wire::{HostCommand, HostCommandResult, ServerFrame};

/// TUI 请求外层执行的副作用。App 本身只更新本地展示状态。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DemoEffect {
    Send(HostCommand),
    Reconnect,
    Quit,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Focus {
    Sessions,
    #[default]
    Input,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum ConnectionStatus {
    #[default]
    Connecting,
    Connected {
        runtime_version: String,
    },
    Disconnected {
        reason: String,
    },
}

/// Demo 只保存终端展示状态；Session、Conversation 与 Run 事实来自 Runtime。
#[derive(Debug, Default)]
pub(crate) struct DemoApp {
    pub(crate) connection: ConnectionStatus,
    pub(crate) configuration_status: Option<ConfigurationStatus>,
    pub(crate) models: Vec<ModelConfiguration>,
    pub(crate) selected_model_key: Option<ModelKey>,
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) selected_session_id: Option<SessionId>,
    pub(crate) conversation: ConversationSnapshot,
    pub(crate) current_run: Option<RunSnapshot>,
    pub(crate) input: Input,
    pub(crate) focus: Focus,
    pub(crate) shutdown_confirmation: bool,
    pub(crate) model_validation_confirmation: bool,
    pub(crate) status_message: String,
    /// 从底部向上回看的视觉行数；0 表示始终跟随最新内容。
    pub(crate) scroll_from_bottom: u16,
}

impl DemoApp {
    pub(crate) fn connected(&mut self, runtime_version: String) -> Vec<DemoEffect> {
        self.connection = ConnectionStatus::Connected {
            runtime_version: runtime_version.clone(),
        };
        self.status_message = format!("Connected to Runtime {runtime_version}");
        vec![
            send_runtime(RuntimeCommand::GetConfigStatus(
                GetConfigStatusRequest::default(),
            )),
            send_runtime(RuntimeCommand::ListModels(ListModelsRequest::default())),
            // Demo 同时承担归档/恢复的人工验证，因此连接后需要保留归档 Session 的入口。
            send_runtime(RuntimeCommand::ListSessions(ListSessionsRequest {
                filter: SessionListFilter::All,
            })),
        ]
    }

    pub(crate) fn connecting(&mut self) {
        self.connection = ConnectionStatus::Connecting;
        self.status_message = "Connecting to Runtime…".to_owned();
    }

    pub(crate) fn disconnected(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.connection = ConnectionStatus::Disconnected {
            reason: reason.clone(),
        };
        self.status_message = format!("Disconnected: {reason}. Press R to reconnect.");
    }

    pub(crate) fn selected_session(&self) -> Option<&SessionSummary> {
        let selected = self.selected_session_id.as_ref()?;
        self.sessions
            .iter()
            .find(|session| &session.session_id == selected)
    }

    pub(crate) fn handle_terminal_event(&mut self, event: &Event) -> Vec<DemoEffect> {
        let Event::Key(key) = event else {
            return Vec::new();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Vec::new();
        }

        if self.shutdown_confirmation {
            return match key.code {
                KeyCode::Enter => {
                    self.shutdown_confirmation = false;
                    self.status_message = "Runtime shutdown requested…".to_owned();
                    vec![send_runtime(RuntimeCommand::ShutdownRuntime(
                        ShutdownRuntimeRequest::default(),
                    ))]
                }
                KeyCode::Esc => {
                    self.shutdown_confirmation = false;
                    self.status_message = "Runtime shutdown cancelled".to_owned();
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }

        if self.model_validation_confirmation {
            return match key.code {
                KeyCode::Enter => {
                    self.model_validation_confirmation = false;
                    self.send_model_validation()
                }
                KeyCode::Esc => {
                    self.model_validation_confirmation = false;
                    self.status_message = "Model validation cancelled".to_owned();
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('q') => {
                    self.shutdown_confirmation = true;
                    return Vec::new();
                }
                KeyCode::Char('c') => return self.cancel_active_run(),
                _ => {}
            }
        }

        if key.code == KeyCode::F(5) {
            if self.is_connected() {
                self.status_message = "Reloading configuration…".to_owned();
                return vec![send_runtime(RuntimeCommand::ReloadConfig(
                    ReloadConfigRequest::default(),
                ))];
            }
            self.status_message = "Reconnect before reloading configuration".to_owned();
            return Vec::new();
        }

        match key.code {
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Sessions => Focus::Input,
                    Focus::Input => Focus::Sessions,
                };
                return Vec::new();
            }
            KeyCode::Esc => {
                self.focus = Focus::Sessions;
                return Vec::new();
            }
            KeyCode::PageUp => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(5);
                return Vec::new();
            }
            KeyCode::PageDown => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(5);
                return Vec::new();
            }
            _ => {}
        }

        match self.focus {
            Focus::Sessions => self.handle_navigation_key(key.code),
            Focus::Input => self.handle_input_key(event, key.code),
        }
    }

    fn handle_navigation_key(&mut self, code: KeyCode) -> Vec<DemoEffect> {
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Char('n' | 'N') => {
                if !self.is_connected() {
                    self.status_message = "Reconnect before creating a Session".to_owned();
                    Vec::new()
                } else {
                    vec![send_runtime(RuntimeCommand::CreateSession(
                        CreateSessionRequest {
                            title: None,
                            model_key: self.selected_model_key.clone(),
                        },
                    ))]
                }
            }
            KeyCode::Char('m' | 'M') => self.select_next_model(),
            KeyCode::Char('s' | 'S') => self.apply_selected_model(),
            KeyCode::Char('a' | 'A') => self.toggle_session_archive(),
            KeyCode::Char('v' | 'V') => self.request_model_validation(),
            KeyCode::Char('r' | 'R') => {
                if matches!(self.connection, ConnectionStatus::Disconnected { .. }) {
                    vec![DemoEffect::Reconnect]
                } else {
                    self.status_message = "Runtime connection is already active".to_owned();
                    Vec::new()
                }
            }
            KeyCode::Char('q' | 'Q') => vec![DemoEffect::Quit],
            _ => Vec::new(),
        }
    }

    fn handle_input_key(&mut self, event: &Event, code: KeyCode) -> Vec<DemoEffect> {
        if code != KeyCode::Enter {
            self.input.handle_event(event);
            return Vec::new();
        }
        let message = self.input.value().to_owned();
        if message.trim().is_empty() {
            self.status_message = "Enter a non-empty message".to_owned();
            return Vec::new();
        }
        if !self.is_connected() {
            self.status_message = "Reconnect before sending a message".to_owned();
            return Vec::new();
        }
        let Some(session_id) = self.selected_session_id.clone() else {
            self.status_message = "Create or select a Session first".to_owned();
            return Vec::new();
        };
        self.input.reset();
        self.scroll_from_bottom = 0;
        self.status_message = "Starting Run…".to_owned();
        vec![send_runtime(RuntimeCommand::SubmitInput(
            SubmitInputRequest {
                session_id,
                message,
                idempotency_key: None,
            },
        ))]
    }

    fn cancel_active_run(&mut self) -> Vec<DemoEffect> {
        let Some(run) = self.current_run.as_ref() else {
            self.status_message = "No active Run to cancel".to_owned();
            return Vec::new();
        };
        if run.status.is_terminal() {
            self.status_message = "The selected Run is already finished".to_owned();
            return Vec::new();
        }
        self.status_message = "Cancelling Run…".to_owned();
        vec![send_runtime(RuntimeCommand::CancelRun(CancelRunRequest {
            session_id: run.session_id.clone(),
            run_id: run.run_id.clone(),
        }))]
    }

    fn move_selection(&mut self, delta: isize) -> Vec<DemoEffect> {
        if self.sessions.is_empty() {
            return Vec::new();
        }
        let current = self
            .selected_session_id
            .as_ref()
            .and_then(|selected| {
                self.sessions
                    .iter()
                    .position(|session| &session.session_id == selected)
            })
            .unwrap_or(0);
        let last = self.sessions.len().saturating_sub(1);
        let next = current.saturating_add_signed(delta).min(last);
        let next_id = self.sessions[next].session_id.clone();
        if self.selected_session_id.as_ref() == Some(&next_id) {
            return Vec::new();
        }
        self.select_session(next_id)
    }

    fn select_session(&mut self, session_id: SessionId) -> Vec<DemoEffect> {
        self.selected_session_id = Some(session_id);
        self.conversation = ConversationSnapshot::default();
        self.current_run = None;
        self.scroll_from_bottom = 0;
        self.selected_refresh_effects()
    }

    fn selected_refresh_effects(&self) -> Vec<DemoEffect> {
        let Some(session_id) = self.selected_session_id.clone() else {
            return Vec::new();
        };
        vec![
            send_runtime(RuntimeCommand::GetSession(GetSessionRequest {
                session_id: session_id.clone(),
            })),
            send_runtime(RuntimeCommand::ListRuns(ListRunsRequest {
                session_id: session_id.clone(),
            })),
            DemoEffect::Send(HostCommand::ConversationSnapshot { session_id }),
        ]
    }

    pub(crate) fn handle_server_frame(&mut self, frame: ServerFrame) -> Vec<DemoEffect> {
        match frame {
            ServerFrame::Response { result, .. } => self.handle_result(result),
            ServerFrame::Error { error, .. } => {
                self.status_message =
                    format!("Request failed: {:?}: {}", error.code, error.message);
                Vec::new()
            }
            ServerFrame::Event { event } => self.handle_runtime_event(event),
            ServerFrame::HelloAck { .. } => Vec::new(),
        }
    }

    fn handle_result(&mut self, result: HostCommandResult) -> Vec<DemoEffect> {
        match result {
            HostCommandResult::ConversationSnapshot { conversation } => {
                self.conversation = conversation;
                self.scroll_from_bottom = 0;
                Vec::new()
            }
            HostCommandResult::Runtime(result) => match result {
                RuntimeCommandResult::GetConfigStatus(result) => {
                    let state = result.status.state;
                    self.configuration_status = Some(result.status);
                    self.status_message = format!("Configuration: {state:?}");
                    Vec::new()
                }
                RuntimeCommandResult::ListModels(result) => {
                    let previous = self.selected_model_key.clone();
                    self.models = result.models;
                    self.selected_model_key = previous
                        .filter(|selected| self.has_model_key(selected))
                        .or_else(|| {
                            self.models
                                .iter()
                                .find(|model| model.is_default)
                                .and_then(|model| model.model_key.clone())
                        })
                        .or_else(|| self.models.iter().find_map(|model| model.model_key.clone()));
                    Vec::new()
                }
                RuntimeCommandResult::GetModel(result) => {
                    self.upsert_model(result.model);
                    Vec::new()
                }
                RuntimeCommandResult::ReloadConfig(result) => {
                    let state = result.status.state;
                    self.configuration_status = Some(result.status);
                    self.status_message = format!("Configuration reloaded: {state:?}");
                    vec![send_runtime(RuntimeCommand::ListModels(
                        ListModelsRequest::default(),
                    ))]
                }
                RuntimeCommandResult::ValidateModelConnection(result) => {
                    self.status_message = match result.outcome {
                        assistant_protocol::ConnectionValidationOutcome::Succeeded => {
                            format!("Model {} connection succeeded", result.model_key)
                        }
                        assistant_protocol::ConnectionValidationOutcome::Failed(failure) => {
                            format!(
                                "Model {} connection failed: {:?}: {}",
                                result.model_key, failure.kind, failure.message
                            )
                        }
                    };
                    Vec::new()
                }
                RuntimeCommandResult::ListSessions(result) => {
                    let previous = self.selected_session_id.clone();
                    self.sessions = result.sessions;
                    if self.sessions.is_empty() && self.can_create_default_session() {
                        self.selected_session_id = None;
                        self.status_message = "Creating the first Session…".to_owned();
                        vec![send_runtime(RuntimeCommand::CreateSession(
                            CreateSessionRequest::default(),
                        ))]
                    } else if self.sessions.is_empty() {
                        self.selected_session_id = None;
                        self.status_message =
                            "No Sessions; fix or reload configuration before creating one"
                                .to_owned();
                        Vec::new()
                    } else {
                        let selected = previous
                            .filter(|id| {
                                self.sessions
                                    .iter()
                                    .any(|session| &session.session_id == id)
                            })
                            .unwrap_or_else(|| self.sessions[0].session_id.clone());
                        self.select_session(selected)
                    }
                }
                RuntimeCommandResult::CreateSession(result) => {
                    let session_id = result.session.session_id.clone();
                    self.upsert_session(result.session);
                    self.status_message = "Session created".to_owned();
                    self.select_session(session_id)
                }
                RuntimeCommandResult::GetSession(result) => {
                    let active_run_id = result.session.active_run_id.clone();
                    let session_id = result.session.session_id.clone();
                    let model_key = result.session.model_key.clone();
                    self.upsert_session(result.session);
                    if self.selected_session_id.as_ref() != Some(&session_id) {
                        return Vec::new();
                    }
                    self.selected_model_key = Some(model_key);
                    active_run_id.map_or_else(Vec::new, |run_id| {
                        vec![send_runtime(RuntimeCommand::GetRun(GetRunRequest {
                            session_id,
                            run_id,
                        }))]
                    })
                }
                RuntimeCommandResult::SubmitInput(result) => {
                    self.status_message = format!("Run {} accepted", result.run.run_id);
                    self.accept_run_snapshot(result.run);
                    self.selected_session_id
                        .clone()
                        .map_or_else(Vec::new, |session_id| {
                            vec![DemoEffect::Send(HostCommand::ConversationSnapshot {
                                session_id,
                            })]
                        })
                }
                RuntimeCommandResult::CancelQueuedInput(result) => {
                    self.status_message = format!("Input {} cancelled", result.input_id);
                    Vec::new()
                }
                RuntimeCommandResult::ResumeSession(result) => {
                    self.upsert_session(result.session);
                    self.status_message = "Session queue resumed".to_owned();
                    Vec::new()
                }
                RuntimeCommandResult::RetryRun(result) => {
                    self.status_message = format!("Run {} accepted", result.run.run_id);
                    self.accept_run_snapshot(result.run);
                    Vec::new()
                }
                RuntimeCommandResult::GetRun(result) => {
                    self.status_message = format!("Run status: {:?}", result.run.status);
                    self.accept_run_snapshot(result.run);
                    Vec::new()
                }
                RuntimeCommandResult::ListRuns(result) => {
                    for run in result.runs {
                        self.accept_run_snapshot(run);
                    }
                    Vec::new()
                }
                RuntimeCommandResult::ArchiveSession(result) => {
                    self.upsert_session(result.session);
                    self.status_message = "Session archived".to_owned();
                    Vec::new()
                }
                RuntimeCommandResult::RestoreSession(result) => {
                    self.upsert_session(result.session);
                    self.status_message = "Session restored".to_owned();
                    Vec::new()
                }
                RuntimeCommandResult::SetSessionModel(result) => {
                    self.upsert_session(result.session);
                    self.status_message = "Session model changed".to_owned();
                    Vec::new()
                }
                RuntimeCommandResult::ReenterFromUserMessage(result) => {
                    self.status_message = format!("Run {} accepted", result.run.run_id);
                    self.accept_run_snapshot(result.run);
                    Vec::new()
                }
                RuntimeCommandResult::CancelRun(result) => {
                    self.status_message = format!("Run status: {:?}", result.run.status);
                    self.accept_run_snapshot(result.run);
                    Vec::new()
                }
                RuntimeCommandResult::ShutdownRuntime(_) => {
                    self.status_message = "Runtime is shutting down".to_owned();
                    vec![DemoEffect::Quit]
                }
            },
        }
    }

    fn handle_runtime_event(&mut self, event: RuntimeEvent) -> Vec<DemoEffect> {
        match event {
            RuntimeEvent::RuntimeShuttingDown => {
                self.status_message = "Runtime is shutting down".to_owned();
                Vec::new()
            }
            RuntimeEvent::SessionCreated { session } => {
                self.upsert_session(session);
                Vec::new()
            }
            RuntimeEvent::RunAccepted { session_id, run_id } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    run.status = RunStatus::Accepted;
                }
                Vec::new()
            }
            RuntimeEvent::RunStarted { session_id, run_id } => {
                self.set_session_active_run(&session_id, Some(run_id.clone()));
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    run.status = RunStatus::Running;
                }
                Vec::new()
            }
            RuntimeEvent::RunCancelling { session_id, run_id } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    run.status = RunStatus::Cancelling;
                    run.cancel_requested = true;
                }
                Vec::new()
            }
            RuntimeEvent::TextDelta {
                session_id,
                run_id,
                delta,
                ..
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    run.text.push_str(&delta);
                    self.scroll_from_bottom = 0;
                }
                Vec::new()
            }
            RuntimeEvent::ReasoningDelta {
                session_id,
                run_id,
                delta,
                ..
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    run.reasoning.push_str(&delta);
                    self.scroll_from_bottom = 0;
                }
                Vec::new()
            }
            RuntimeEvent::ToolProposed {
                session_id,
                run_id,
                call_id,
                tool_name,
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    let tool = ensure_tool(run, call_id);
                    tool.tool_name = tool_name;
                    tool.status = ToolActivityStatus::Proposed;
                }
                Vec::new()
            }
            RuntimeEvent::ToolStarted {
                session_id,
                run_id,
                call_id,
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    ensure_tool(run, call_id).status = ToolActivityStatus::Running;
                }
                Vec::new()
            }
            RuntimeEvent::ToolOutput {
                session_id,
                run_id,
                call_id,
                channel,
                chunk,
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    let tool = ensure_tool(run, call_id);
                    match channel {
                        ToolOutputChannel::Stdout => tool.stdout.push_str(&chunk),
                        ToolOutputChannel::Stderr => tool.stderr.push_str(&chunk),
                    }
                }
                Vec::new()
            }
            RuntimeEvent::ToolCompleted {
                session_id,
                run_id,
                call_id,
                status,
            } => {
                if let Some(run) = self.selected_run_mut(session_id, run_id) {
                    ensure_tool(run, call_id).status = status;
                }
                Vec::new()
            }
            RuntimeEvent::RunFinished {
                session_id,
                run_id,
                status,
                error,
            } => {
                self.set_session_active_run(&session_id, None);
                let selected = self.selected_session_id.as_ref() == Some(&session_id);
                if let Some(run) = self.selected_run_mut(session_id.clone(), run_id.clone()) {
                    run.status = status;
                    run.error = error;
                    self.status_message = format!("Run {run_id} finished: {status:?}");
                }
                if !selected {
                    return Vec::new();
                }
                vec![
                    send_runtime(RuntimeCommand::GetRun(GetRunRequest {
                        session_id: session_id.clone(),
                        run_id,
                    })),
                    send_runtime(RuntimeCommand::GetSession(GetSessionRequest {
                        session_id: session_id.clone(),
                    })),
                    DemoEffect::Send(HostCommand::ConversationSnapshot { session_id }),
                ]
            }
        }
    }

    fn selected_run_mut(
        &mut self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Option<&mut RunSnapshot> {
        if self.selected_session_id.as_ref() != Some(&session_id) {
            return None;
        }
        let replace = self
            .current_run
            .as_ref()
            .is_none_or(|run| run.session_id != session_id || run.run_id != run_id);
        if replace {
            self.current_run = Some(empty_run(session_id, run_id));
        }
        self.current_run.as_mut()
    }

    fn accept_run_snapshot(&mut self, run: RunSnapshot) {
        if self.selected_session_id.as_ref() == Some(&run.session_id) {
            self.current_run = Some(run);
        }
    }

    fn upsert_session(&mut self, session: SessionSummary) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.session_id == session.session_id)
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    fn upsert_model(&mut self, model: ModelConfiguration) {
        if let Some(existing) = self
            .models
            .iter_mut()
            .find(|existing| existing.model_key == model.model_key)
        {
            *existing = model;
        } else {
            self.models.push(model);
        }
    }

    fn select_next_model(&mut self) -> Vec<DemoEffect> {
        let keys: Vec<_> = self
            .models
            .iter()
            .filter_map(|model| model.model_key.clone())
            .collect();
        if keys.is_empty() {
            self.status_message = "No model configuration is available".to_owned();
            return Vec::new();
        }
        let next = self
            .selected_model_key
            .as_ref()
            .and_then(|selected| keys.iter().position(|key| key == selected))
            .map_or(0, |index| (index + 1) % keys.len());
        let selected = keys[next].clone();
        self.status_message = format!("Selected model {selected}");
        self.selected_model_key = Some(selected);
        Vec::new()
    }

    fn apply_selected_model(&mut self) -> Vec<DemoEffect> {
        if !self.is_connected() {
            self.status_message = "Reconnect before changing the Session model".to_owned();
            return Vec::new();
        }
        let Some(session_id) = self.selected_session_id.clone() else {
            self.status_message = "Create or select a Session first".to_owned();
            return Vec::new();
        };
        let Some(model_key) = self.selected_model_key.clone() else {
            self.status_message = "Select a model first".to_owned();
            return Vec::new();
        };
        self.status_message = format!("Changing Session model to {model_key}…");
        vec![send_runtime(RuntimeCommand::SetSessionModel(
            SetSessionModelRequest {
                session_id,
                model_key,
            },
        ))]
    }

    fn toggle_session_archive(&mut self) -> Vec<DemoEffect> {
        if !self.is_connected() {
            self.status_message = "Reconnect before changing Session lifecycle".to_owned();
            return Vec::new();
        }
        let Some(session) = self.selected_session() else {
            self.status_message = "Create or select a Session first".to_owned();
            return Vec::new();
        };
        let session_id = session.session_id.clone();
        match session.lifecycle {
            SessionLifecycle::Active => {
                self.status_message = "Archiving Session…".to_owned();
                vec![send_runtime(RuntimeCommand::ArchiveSession(
                    ArchiveSessionRequest { session_id },
                ))]
            }
            SessionLifecycle::Archived => {
                self.status_message = "Restoring Session…".to_owned();
                vec![send_runtime(RuntimeCommand::RestoreSession(
                    RestoreSessionRequest { session_id },
                ))]
            }
        }
    }

    fn request_model_validation(&mut self) -> Vec<DemoEffect> {
        if !self.is_connected() {
            self.status_message = "Reconnect before validating a model".to_owned();
            return Vec::new();
        }
        let Some(model_key) = self.selected_model_key.clone() else {
            self.status_message = "Select a model before validation".to_owned();
            return Vec::new();
        };
        self.model_validation_confirmation = true;
        self.status_message = format!("Confirm validation for model {model_key}");
        Vec::new()
    }

    fn send_model_validation(&mut self) -> Vec<DemoEffect> {
        let Some(model_key) = self.selected_model_key.clone() else {
            self.status_message = "Selected model is no longer available".to_owned();
            return Vec::new();
        };
        self.status_message = format!("Validating model {model_key}…");
        vec![send_runtime(RuntimeCommand::ValidateModelConnection(
            ValidateModelConnectionRequest { model_key },
        ))]
    }

    fn has_model_key(&self, model_key: &ModelKey) -> bool {
        self.models
            .iter()
            .any(|model| model.model_key.as_ref() == Some(model_key))
    }

    fn set_session_active_run(&mut self, session_id: &SessionId, run_id: Option<RunId>) {
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| &session.session_id == session_id)
        {
            session.active_run_id = run_id;
        }
    }

    fn is_connected(&self) -> bool {
        matches!(self.connection, ConnectionStatus::Connected { .. })
    }

    fn can_create_default_session(&self) -> bool {
        self.configuration_status.as_ref().is_some_and(|status| {
            status.state == ConfigurationState::Ready
                || (status.state == ConfigurationState::Degraded
                    && self
                        .models
                        .iter()
                        .any(|model| model.is_default && model.is_valid))
        })
    }
}

fn send_runtime(command: RuntimeCommand) -> DemoEffect {
    DemoEffect::Send(HostCommand::Runtime(command))
}

fn empty_run(session_id: SessionId, run_id: RunId) -> RunSnapshot {
    RunSnapshot {
        run_id,
        session_id,
        input_id: assistant_protocol::InputId::new("i_1").expect("input"),
        attempt: 1,
        status: RunStatus::Accepted,
        cancel_requested: false,
        reasoning: String::new(),
        text: String::new(),
        tools: Vec::new(),
        error: None,
    }
}

fn ensure_tool(run: &mut RunSnapshot, call_id: ToolCallId) -> &mut ToolActivitySnapshot {
    if let Some(index) = run.tools.iter().position(|tool| tool.call_id == call_id) {
        return &mut run.tools[index];
    }
    run.tools.push(ToolActivitySnapshot {
        call_id,
        tool_name: "unknown".to_owned(),
        status: ToolActivityStatus::Proposed,
        stdout: String::new(),
        stderr: String::new(),
    });
    run.tools.last_mut().expect("tool was inserted")
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{
        CreateSessionResult, ListSessionsResult, ModelKey, RuntimeErrorCode, RuntimeErrorInfo,
        SubmitInputResult,
    };
    use crossterm::event::KeyEvent;

    use super::*;

    fn session(id: &str, title: &str) -> SessionSummary {
        SessionSummary {
            session_id: SessionId::new(id).expect("session id"),
            title: title.to_owned(),
            model_key: ModelKey::new("model-1").expect("model key"),
            lifecycle: assistant_protocol::SessionLifecycle::Active,
            active_run_id: None,
            queued_input_count: 0,
            resume_required: false,
            message_count: 0,
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn response(result: RuntimeCommandResult) -> ServerFrame {
        ServerFrame::Response {
            request_id: "request-1".to_owned(),
            result: HostCommandResult::Runtime(result),
        }
    }

    fn configuration_status(state: ConfigurationState) -> ConfigurationStatus {
        ConfigurationStatus {
            config_path: Some("/private/runtime/config.toml".to_owned()),
            state,
            schema_version: Some(1),
            default_model: Some(ModelKey::new("model-1").expect("model key")),
            issues: Vec::new(),
        }
    }

    fn model(key: &str, is_default: bool, is_valid: bool) -> ModelConfiguration {
        ModelConfiguration {
            model_key: Some(ModelKey::new(key).expect("model key")),
            display_name: key.to_owned(),
            protocol: Some("chat_completions".to_owned()),
            provider: Some("fixture".to_owned()),
            endpoint: Some("https://api.example.test/v1".to_owned()),
            model: Some(format!("{key}-model")),
            context_window_tokens: Some(8_192),
            max_output_tokens: Some(4_096),
            agent_max_output_tokens: None,
            effective_max_output_tokens: Some(4_096),
            api_key_configured: true,
            is_default,
            is_valid,
            issues: Vec::new(),
        }
    }

    #[test]
    fn empty_runtime_creates_a_session_and_nonempty_list_selects_without_manual_ids() {
        let mut app = DemoApp {
            configuration_status: Some(configuration_status(ConfigurationState::Ready)),
            ..DemoApp::default()
        };
        let effects = app.handle_server_frame(response(RuntimeCommandResult::ListSessions(
            ListSessionsResult { sessions: vec![] },
        )));
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::CreateSession(_)
            ))]
        ));

        let effects = app.handle_server_frame(response(RuntimeCommandResult::ListSessions(
            ListSessionsResult {
                sessions: vec![session("s_1", "First")],
            },
        )));
        assert_eq!(
            app.selected_session_id.as_ref().map(ToString::to_string),
            Some("s_1".to_owned())
        );
        assert_eq!(effects.len(), 3);
        assert!(matches!(
            effects[0],
            DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::GetSession(_)))
        ));
        assert!(matches!(
            effects[1],
            DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::ListRuns(_)))
        ));
        assert!(matches!(
            effects[2],
            DemoEffect::Send(HostCommand::ConversationSnapshot { .. })
        ));
    }

    #[test]
    fn connection_queries_configuration_and_f5_explicitly_reloads_it() {
        let mut app = DemoApp::default();
        let effects = app.connected("0.1.0".to_owned());
        assert!(matches!(
            effects.as_slice(),
            [
                DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::GetConfigStatus(_))),
                DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::ListModels(_))),
                DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::ListSessions(
                    ListSessionsRequest {
                        filter: SessionListFilter::All
                    }
                )))
            ]
        ));

        let effects = app.handle_terminal_event(&key(KeyCode::F(5), KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::ReloadConfig(_)
            ))]
        ));
        let effects = app.handle_server_frame(response(RuntimeCommandResult::ReloadConfig(
            assistant_protocol::ReloadConfigResult {
                status: configuration_status(ConfigurationState::Degraded),
            },
        )));
        assert_eq!(
            app.configuration_status.as_ref().map(|status| status.state),
            Some(ConfigurationState::Degraded)
        );
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::ListModels(_)
            ))]
        ));
    }

    #[test]
    fn model_selection_drives_explicit_session_creation_and_confirmed_validation() {
        let mut app = DemoApp {
            connection: ConnectionStatus::Connected {
                runtime_version: "0.1.0".to_owned(),
            },
            focus: Focus::Sessions,
            ..DemoApp::default()
        };
        app.handle_server_frame(response(RuntimeCommandResult::ListModels(
            assistant_protocol::ListModelsResult {
                models: vec![model("model-1", true, true), model("model-2", false, true)],
            },
        )));
        assert_eq!(
            app.selected_model_key.as_ref().map(ToString::to_string),
            Some("model-1".to_owned())
        );

        assert!(
            app.handle_terminal_event(&key(KeyCode::Char('m'), KeyModifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            app.selected_model_key.as_ref().map(ToString::to_string),
            Some("model-2".to_owned())
        );

        assert!(
            app.handle_terminal_event(&key(KeyCode::Char('v'), KeyModifiers::NONE))
                .is_empty()
        );
        assert!(app.model_validation_confirmation);
        let effects = app.handle_terminal_event(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest {
                    model_key
                })
            ))] if model_key.as_str() == "model-2"
        ));

        let effects = app.handle_terminal_event(&key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::CreateSession(
                CreateSessionRequest {
                    model_key: Some(model_key),
                    ..
                }
            )))] if model_key.as_str() == "model-2"
        ));
    }

    #[test]
    fn unicode_input_sends_directly_and_q_is_text_while_input_is_focused() {
        let mut app = DemoApp::default();
        app.connection = ConnectionStatus::Connected {
            runtime_version: "0.1.0".to_owned(),
        };
        app.sessions = vec![session("s_1", "First")];
        app.selected_session_id = Some(app.sessions[0].session_id.clone());

        assert!(
            app.handle_terminal_event(&key(KeyCode::Char('你'), KeyModifiers::NONE))
                .is_empty()
        );
        assert!(
            app.handle_terminal_event(&key(KeyCode::Char('q'), KeyModifiers::NONE))
                .is_empty()
        );
        let effects = app.handle_terminal_event(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::SubmitInput(
                SubmitInputRequest { message, .. }
            )))] if message == "你q"
        ));
        assert!(app.input.value().is_empty());
    }

    #[test]
    fn navigation_can_apply_model_and_toggle_archive_state() {
        let mut app = DemoApp {
            connection: ConnectionStatus::Connected {
                runtime_version: "0.1.0".to_owned(),
            },
            focus: Focus::Sessions,
            sessions: vec![session("s_1", "First")],
            selected_session_id: Some(SessionId::new("s_1").expect("session")),
            selected_model_key: Some(ModelKey::new("model-2").expect("model")),
            ..DemoApp::default()
        };

        assert!(matches!(
            app.handle_terminal_event(&key(KeyCode::Char('s'), KeyModifiers::NONE))
                .as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(RuntimeCommand::SetSessionModel(
                SetSessionModelRequest { model_key, .. }
            )))] if model_key.as_str() == "model-2"
        ));
        assert!(matches!(
            app.handle_terminal_event(&key(KeyCode::Char('a'), KeyModifiers::NONE))
                .as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::ArchiveSession(_)
            ))]
        ));

        app.sessions[0].lifecycle = SessionLifecycle::Archived;
        assert!(matches!(
            app.handle_terminal_event(&key(KeyCode::Char('a'), KeyModifiers::NONE))
                .as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::RestoreSession(_)
            ))]
        ));
    }

    #[test]
    fn navigation_quit_and_runtime_shutdown_are_distinct() {
        let mut app = DemoApp {
            focus: Focus::Sessions,
            ..DemoApp::default()
        };
        assert_eq!(
            app.handle_terminal_event(&key(KeyCode::Char('q'), KeyModifiers::NONE)),
            vec![DemoEffect::Quit]
        );
        assert!(
            app.handle_terminal_event(&key(KeyCode::Char('q'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(app.shutdown_confirmation);
        assert!(matches!(
            app.handle_terminal_event(&key(KeyCode::Enter, KeyModifiers::NONE))
                .as_slice(),
            [DemoEffect::Send(HostCommand::Runtime(
                RuntimeCommand::ShutdownRuntime(_)
            ))]
        ));
    }

    #[test]
    fn disconnect_keeps_snapshot_and_manual_reconnect_is_explicit() {
        let mut app = DemoApp {
            focus: Focus::Sessions,
            sessions: vec![session("s_1", "First")],
            selected_session_id: Some(SessionId::new("s_1").expect("session")),
            ..DemoApp::default()
        };
        app.disconnected("socket closed");
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(
            app.handle_terminal_event(&key(KeyCode::Char('r'), KeyModifiers::NONE)),
            vec![DemoEffect::Reconnect]
        );
    }

    #[test]
    fn run_events_update_projection_and_finish_requests_authoritative_snapshots() {
        let mut app = DemoApp::default();
        app.sessions = vec![session("s_1", "First")];
        app.selected_session_id = Some(app.sessions[0].session_id.clone());
        let session_id = app.sessions[0].session_id.clone();
        let run_id = RunId::new("r_1").expect("run");
        let run = empty_run(session_id.clone(), run_id.clone());
        app.handle_server_frame(response(RuntimeCommandResult::SubmitInput(
            SubmitInputResult {
                input_id: run.input_id.clone(),
                run,
            },
        )));
        app.handle_server_frame(ServerFrame::Event {
            event: RuntimeEvent::TextDelta {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                part_id: assistant_protocol::PartId::new("p_1").expect("part"),
                delta: "hello".to_owned(),
            },
        });
        let effects = app.handle_server_frame(ServerFrame::Event {
            event: RuntimeEvent::RunFinished {
                session_id,
                run_id,
                status: RunStatus::Completed,
                error: None,
            },
        });
        assert_eq!(
            app.current_run.as_ref().map(|run| run.text.as_str()),
            Some("hello")
        );
        assert_eq!(
            app.current_run.as_ref().map(|run| run.status),
            Some(RunStatus::Completed)
        );
        assert_eq!(effects.len(), 3);
    }

    #[test]
    fn events_from_an_unselected_session_do_not_replace_the_visible_run() {
        let mut app = DemoApp {
            sessions: vec![session("s_1", "First"), session("s_2", "Second")],
            ..DemoApp::default()
        };
        let first = app.sessions[0].session_id.clone();
        let second = app.sessions[1].session_id.clone();
        app.selected_session_id = Some(first.clone());
        app.current_run = Some(empty_run(first, RunId::new("r_1").expect("selected run")));

        app.handle_server_frame(ServerFrame::Event {
            event: RuntimeEvent::TextDelta {
                session_id: second,
                run_id: RunId::new("r_2").expect("background run"),
                part_id: assistant_protocol::PartId::new("p_1").expect("part"),
                delta: "background".to_owned(),
            },
        });

        let visible = app.current_run.expect("visible run");
        assert_eq!(visible.run_id.to_string(), "r_1");
        assert!(visible.text.is_empty());
    }

    #[test]
    fn safe_runtime_error_is_projected_without_changing_local_facts() {
        let mut app = DemoApp::default();
        app.handle_server_frame(ServerFrame::Error {
            request_id: "request-1".to_owned(),
            error: RuntimeErrorInfo::new(RuntimeErrorCode::SessionBusy, "session is busy"),
        });
        assert!(app.status_message.contains("session is busy"));
        assert!(app.sessions.is_empty());
    }

    #[test]
    fn create_session_response_selects_and_refreshes_it() {
        let mut app = DemoApp::default();
        let effects = app.handle_server_frame(response(RuntimeCommandResult::CreateSession(
            CreateSessionResult {
                session: session("s_1", "First"),
            },
        )));
        assert_eq!(
            app.selected_session_id.as_ref().map(ToString::to_string),
            Some("s_1".to_owned())
        );
        assert_eq!(effects.len(), 3);
    }
}
