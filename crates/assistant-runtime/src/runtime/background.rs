//! 用户域内的后台 Shell 登记与监督。Run 只拥有交接前的启动和首次等待。

pub(super) mod tools;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, RwLock},
    time::Duration,
};

use agent_tools::{
    BackgroundOutput, BackgroundOutputRequest, BackgroundShell, BackgroundShellFuture,
    BackgroundShellLauncher, ShellToolError,
};
pub use agent_tools::{BackgroundRunExit, BackgroundShellStart, StartBackgroundShell};
use agent_types::ToolCallId;
use assistant_protocol::{
    BackgroundRunStatus, BackgroundRunSummary, ListBackgroundRunsRequest, ListBackgroundRunsResult,
    ReadBackgroundOutputRequest, ReadBackgroundOutputResult, RunId, RuntimeEvent, SessionId,
    StopBackgroundRunRequest, StopBackgroundRunResult, WorkspaceId,
};
use tokio::{
    sync::{oneshot, watch},
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;

use super::{AssistantRuntime, tasks::RuntimeTasks};
use crate::{
    RuntimeError, RuntimeResult, StoredWorkspace, StoredWorkspaceLifecycle,
    observation::ObservationCoordinator, session::SessionController,
};

const MAX_ACTIVE: usize = 16;
const MAX_INITIAL_WAIT: Duration = Duration::from_secs(10);
const DEFAULT_INITIAL_WAIT: Duration = Duration::from_secs(1);

/// 来源由已接纳的执行路径传入，调用方不能用模型参数指定归属。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundRunOrigin {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
}

#[derive(Clone)]
pub(super) struct BackgroundRunServices {
    runs: Arc<BackgroundRuns>,
    operation_gate: Arc<tokio::sync::RwLock<()>>,
    workspaces: Arc<RwLock<BTreeMap<WorkspaceId, StoredWorkspace>>>,
    cancellation: CancellationToken,
    tasks: Arc<RuntimeTasks>,
    stop_timeout: Duration,
}

pub(super) struct BoundBackgroundShell {
    services: BackgroundRunServices,
    session: Arc<SessionController>,
    run_id: RunId,
    shell: Arc<dyn BackgroundShell>,
}

impl BackgroundShellLauncher for BoundBackgroundShell {
    fn launch(
        &self,
        call_id: ToolCallId,
        request: StartBackgroundShell,
        cancellation: CancellationToken,
    ) -> BackgroundShellFuture<'_, BackgroundShellStart> {
        Box::pin(async move {
            // Recorder 先可靠写入 exchange，再安装 pending。没有这份接纳事实的历史或重复
            // 回放不能启动进程，活动登记只负责合并仍未结算的同一调用。
            let pending = self
                .session
                .lock_state()
                .map_err(|_| invalid("background session is unavailable"))?
                .journal
                .as_ref()
                .is_some_and(|journal| {
                    journal.has_pending_tool_call(&self.run_id, &call_id, "shell")
                });
            if !pending {
                return Err(invalid("background shell requires a pending tool call"));
            }
            self.services
                .start(
                    &self.session,
                    BackgroundRunOrigin {
                        session_id: self.session.id().clone(),
                        run_id: self.run_id.clone(),
                        call_id,
                    },
                    self.shell.clone(),
                    request,
                    cancellation,
                )
                .await
        })
    }
}

pub(super) struct BackgroundRuns {
    active: Mutex<BTreeMap<String, Arc<Entry>>>,
    events: ObservationCoordinator,
}

struct Entry {
    id: String,
    origin: BackgroundRunOrigin,
    workspace: Option<WorkspaceId>,
    command: String,
    working_directory: String,
    started_at_ms: i64,
    stop: watch::Sender<bool>,
    scheduled: Option<Arc<super::schedule::ScheduledSession>>,
    state: watch::Sender<State>,
}

#[derive(Clone)]
enum State {
    Starting,
    Running(Arc<dyn BackgroundOutput>),
    CleanupFailed(Option<Arc<dyn BackgroundOutput>>, ShellToolError),
    Finished(Result<BackgroundRunExit, ShellToolError>),
}

impl State {
    fn status(&self) -> Option<BackgroundRunStatus> {
        match self {
            Self::Running(_) => Some(BackgroundRunStatus::Running),
            Self::CleanupFailed(_, _) => Some(BackgroundRunStatus::CleanupUnconfirmed),
            Self::Starting | Self::Finished(_) => None,
        }
    }

    fn output(&self) -> Option<&Arc<dyn BackgroundOutput>> {
        match self {
            Self::Running(output) | Self::CleanupFailed(Some(output), _) => Some(output),
            Self::Starting | Self::CleanupFailed(None, _) | Self::Finished(_) => None,
        }
    }
}

impl AssistantRuntime {
    pub(super) fn background_services(&self) -> BackgroundRunServices {
        BackgroundRunServices {
            runs: self.background_runs.clone(),
            operation_gate: self.operation_gate.clone(),
            workspaces: self.workspaces.clone(),
            cancellation: self.root_cancellation.clone(),
            tasks: self.tasks.clone(),
            stop_timeout: self.config.shutdown_timeout,
        }
    }

    pub fn list_background_runs(
        &self,
        request: ListBackgroundRunsRequest,
    ) -> RuntimeResult<ListBackgroundRunsResult> {
        super::schedule::agent_visibility(&request.session_id)?;
        self.background_runs.list(request)
    }

    pub async fn read_background_output(
        &self,
        request: ReadBackgroundOutputRequest,
    ) -> RuntimeResult<ReadBackgroundOutputResult> {
        super::schedule::agent_visibility(&request.session_id)?;
        self.background_runs.read(request).await
    }

    pub async fn stop_background_run(
        &self,
        request: StopBackgroundRunRequest,
    ) -> RuntimeResult<StopBackgroundRunResult> {
        super::schedule::agent_visibility(&request.session_id)?;
        self.background_services().stop(request).await
    }

    #[cfg(test)]
    pub async fn start_background_shell(
        &self,
        origin: BackgroundRunOrigin,
        request: StartBackgroundShell,
        cancellation: CancellationToken,
    ) -> Result<BackgroundShellStart, ShellToolError> {
        let session = self
            .prepared_session(&origin.session_id)
            .await
            .map_err(|_| invalid("background session is unavailable"))?;
        let frozen_shell = session
            .lock_state()
            .map_err(|_| invalid("background session is unavailable"))?
            .agent_shell_environment
            .clone();
        let shell = self
            .run_tool_factory
            .background_shell(frozen_shell.as_ref())
            .ok_or_else(|| invalid("background shell is unavailable"))?;
        self.background_services()
            .start(&session, origin, shell, request, cancellation)
            .await
    }

    #[cfg(test)]
    pub fn background_shell_ids(&self, session_id: &SessionId) -> Vec<String> {
        self.background_runs
            .entries()
            .values()
            .filter(|entry| &entry.origin.session_id == session_id)
            .filter(|entry| !matches!(*entry.state.borrow(), State::Starting))
            .map(|entry| entry.id.clone())
            .collect()
    }

    #[cfg(test)]
    pub async fn read_background_shell(
        &self,
        session_id: &SessionId,
        id: &str,
        request: BackgroundOutputRequest,
    ) -> Result<agent_tools::BackgroundOutputPage, ShellToolError> {
        let entry = self.background_runs.find(session_id, id)?;
        let output = entry
            .state
            .borrow()
            .output()
            .cloned()
            .ok_or_else(not_found)?;
        output.read(request).await
    }

    #[cfg(test)]
    pub async fn stop_background_shell(
        &self,
        session_id: &SessionId,
        id: &str,
    ) -> Result<BackgroundRunExit, ShellToolError> {
        let entry = self.background_runs.find(session_id, id)?;
        timeout(self.config.shutdown_timeout, entry.stop())
            .await
            .unwrap_or_else(|_| {
                Err(ShellToolError::CleanupUnconfirmed {
                    message: "background process cleanup timed out".into(),
                })
            })
    }
}

impl BackgroundRunServices {
    pub(super) fn bind(
        &self,
        session: Arc<SessionController>,
        run_id: RunId,
        shell: Arc<dyn BackgroundShell>,
    ) -> BoundBackgroundShell {
        BoundBackgroundShell {
            services: self.clone(),
            session,
            run_id,
            shell,
        }
    }

    pub(super) fn runs(&self) -> &Arc<BackgroundRuns> {
        &self.runs
    }

    /// 接纳先锁操作门，再核对当前 Session、Run 和工作区。监督者不持此门等待进程，
    /// 因此启动、删除与关闭互斥，运行和停止不会阻塞相应持久化操作。
    async fn start(
        &self,
        session: &SessionController,
        origin: BackgroundRunOrigin,
        shell: Arc<dyn BackgroundShell>,
        request: StartBackgroundShell,
        cancellation: CancellationToken,
    ) -> Result<BackgroundShellStart, ShellToolError> {
        let wait = request.wait.unwrap_or(DEFAULT_INITIAL_WAIT);
        if wait > MAX_INITIAL_WAIT {
            return Err(invalid("background wait must be at most 10000 ms"));
        }
        if request.timeout.is_some_and(|duration| {
            duration.is_zero() || Instant::now().checked_add(duration).is_none()
        }) {
            return Err(invalid(
                "background total timeout is outside the supported range",
            ));
        }
        let (entry, _handoff) = {
            let _operation = self.operation_gate.read().await;
            if self.cancellation.is_cancelled() || cancellation.is_cancelled() {
                return Err(ShellToolError::Cancelled);
            }
            session
                .ensure_standard_role()
                .and_then(|_| session.ensure_active())
                .map_err(|_| invalid("background shell requires an active main session"))?;
            let eligible = session
                .lock_state()
                .map_err(|_| invalid("background session is unavailable"))?
                .active_run
                .as_ref()
                .is_some_and(|run| run.run_id == origin.run_id && !run.cancellation.is_cancelled());
            if !eligible {
                return Err(invalid("background shell requires the active Run"));
            }
            let workspace = session.environment().workspace_id.clone();
            if let Some(id) = &workspace {
                let workspaces = self
                    .workspaces
                    .read()
                    .map_err(|_| invalid("background workspace is unavailable"))?;
                if !workspaces
                    .get(id)
                    .is_some_and(|entry| entry.lifecycle == StoredWorkspaceLifecycle::Active)
                {
                    return Err(invalid("background workspace is unavailable"));
                }
            }
            let (entry, new) =
                self.runs
                    .reserve(origin, workspace, &request, session.scheduled.clone())?;
            // 只有 context 仍在等待、且取消未生效时允许移交。现有工具记录负责拦截已结算调用，
            // 此处按真实 call ID 只合并活动启动，结果未知时不得通过重放创建新的进程。
            let (handoff, receipt) = oneshot::channel();
            if new {
                let owner = self.runs.clone();
                let supervisor = entry.clone();
                let shutdown = self.cancellation.clone();
                let launch_cancellation = cancellation.clone();
                let on_panic = entry.clone();
                let panic_owner = owner.clone();
                self.tasks.spawn(
                    async move {
                        owner
                            .supervise(
                                supervisor,
                                shell,
                                request,
                                handoff,
                                launch_cancellation,
                                shutdown,
                            )
                            .await;
                    },
                    async move {
                        let output = on_panic.state.borrow().output().cloned();
                        on_panic.update(
                            State::CleanupFailed(
                                output,
                                ShellToolError::CleanupUnconfirmed {
                                    message: "background supervisor stopped unexpectedly".into(),
                                },
                            ),
                            &panic_owner.events,
                        );
                    },
                );
            }
            (entry, receipt)
        };
        entry.initial_result(wait, cancellation).await
    }

    pub(super) async fn stop(
        &self,
        request: StopBackgroundRunRequest,
    ) -> RuntimeResult<StopBackgroundRunResult> {
        let entry = self
            .runs
            .visible(&request.session_id, &request.background_id)?;
        let result = timeout(self.stop_timeout, entry.stop())
            .await
            .map_err(|_| RuntimeError::BackgroundCleanupUnconfirmed)?
            .map_err(|_| RuntimeError::BackgroundCleanupUnconfirmed)?;
        Ok(entry.exit_result(&result))
    }
}

impl BackgroundRuns {
    pub(super) fn new(events: ObservationCoordinator) -> Self {
        Self {
            active: Mutex::default(),
            events,
        }
    }

    fn reserve(
        &self,
        origin: BackgroundRunOrigin,
        workspace: Option<WorkspaceId>,
        request: &StartBackgroundShell,
        scheduled: Option<Arc<super::schedule::ScheduledSession>>,
    ) -> Result<(Arc<Entry>, bool), ShellToolError> {
        let mut active = self.entries();
        if let Some(existing) = active.values().find(|entry| entry.origin == origin) {
            return Ok((existing.clone(), false));
        }
        if active.len() >= MAX_ACTIVE {
            return Err(invalid("background shell capacity reached"));
        }
        let id = crate::id::generate_unique("background", |id| active.contains_key(id))
            .map_err(|_| invalid("could not allocate a background shell identifier"))?;
        let entry = Arc::new(Entry {
            id: id.clone(),
            origin,
            workspace,
            command: request.shell.command.clone(),
            working_directory: request.shell.workdir.as_str().to_owned(),
            started_at_ms: super::now_ms()
                .map_err(|_| invalid("background start time is unavailable"))?,
            stop: watch::channel(false).0,
            scheduled,
            state: watch::channel(State::Starting).0,
        });
        active.insert(id, entry.clone());
        Ok((entry, true))
    }

    async fn supervise(
        &self,
        entry: Arc<Entry>,
        shell: Arc<dyn BackgroundShell>,
        request: StartBackgroundShell,
        handoff: oneshot::Sender<()>,
        cancellation: CancellationToken,
        shutdown: CancellationToken,
    ) {
        let mut stop = entry.stop.subscribe();
        if cancellation.is_cancelled() || shutdown.is_cancelled() || handoff.is_closed() {
            self.finish(&entry, Err(ShellToolError::Cancelled));
            return;
        }
        let started = Instant::now();
        let mut process = match shell.start(&entry.id, request.shell).await {
            Ok(process) => process,
            Err(error @ ShellToolError::CleanupUnconfirmed { .. }) => {
                entry.update(State::CleanupFailed(None, error), &self.events);
                return;
            }
            Err(error) => {
                self.finish(&entry, Err(error));
                return;
            }
        };
        let output = process.output();
        let accepted =
            !cancellation.is_cancelled() && !shutdown.is_cancelled() && handoff.send(()).is_ok();
        let timed_out = if accepted {
            entry.update(State::Running(output.clone()), &self.events);
            let deadline = request
                .timeout
                .and_then(|duration| started.checked_add(duration));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => false,
                _ = stop.wait_for(|stop| *stop) => false,
                outcome = process.wait_parent() => {
                    if let Err(error) = outcome {
                        eprintln!("background parent wait failed for {}: {error}", entry.id);
                    }
                    false
                },
                _ = async {
                    if let Some(deadline) = deadline {
                        sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => true,
            }
        } else {
            false
        };
        loop {
            match process.finish().await {
                Ok(process) => {
                    let result = if accepted {
                        Ok(BackgroundRunExit { process, timed_out })
                    } else {
                        Err(ShellToolError::Cancelled)
                    };
                    self.finish(&entry, result);
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "background process cleanup failed for {}: {error}",
                        entry.id
                    );
                    entry.update(
                        State::CleanupFailed(accepted.then(|| output.clone()), error),
                        &self.events,
                    );
                }
            }
            if shutdown.is_cancelled() {
                return;
            }
            // 用户重试或 Runtime 关闭都由原监督者结算，失败不释放容量或身份文件。
            tokio::select! {
                _ = shutdown.cancelled() => {}
                _ = stop.changed() => {}
            }
        }
    }

    fn finish(&self, entry: &Entry, result: Result<BackgroundRunExit, ShellToolError>) {
        // 先移除活动事实，再发布一次终态，事件不携带日志正文。已有等待者继续共享实际结果。
        {
            let mut entries = self.entries();
            if let Some(task) = &entry.scheduled {
                let exit = assistant_protocol::ScheduleBackgroundExit {
                    background_id: entry.id.clone(),
                    exit_code: result
                        .as_ref()
                        .ok()
                        .and_then(|result| result.process.exit_code),
                    timed_out: result.as_ref().is_ok_and(|result| result.timed_out),
                    stopped: *entry.stop.borrow(),
                };
                task.background_exits
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(exit);
            }
            entries.remove(&entry.id);
        }
        let event = result
            .as_ref()
            .ok()
            .map(|result| RuntimeEvent::BackgroundRunEnded {
                session_id: entry.origin.session_id.clone(),
                run_id: entry.origin.run_id.clone(),
                result: entry.exit_result(result),
            });
        entry.state.send_replace(State::Finished(result));
        if let Some(task) = &entry.scheduled {
            task.background_changed.send_replace(());
        }
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
    }

    pub(super) fn list(
        &self,
        request: ListBackgroundRunsRequest,
    ) -> RuntimeResult<ListBackgroundRunsResult> {
        let offset = request.offset.unwrap_or(0);
        let limit = request.limit.unwrap_or(10);
        if !(1..=50).contains(&limit) {
            return Err(RuntimeError::InvalidRequest {
                reason: "background list limit must be between 1 and 50",
            });
        }
        let include_command = request.include_command.unwrap_or(false);
        if include_command && request.background_id.is_none() {
            return Err(RuntimeError::InvalidRequest {
                reason: "full background command requires a background id",
            });
        }
        let entries = self.entries();
        let mut runs: Vec<_> = entries
            .values()
            .filter(|entry| entry.origin.session_id == request.session_id)
            .filter(|entry| {
                request
                    .background_id
                    .as_ref()
                    .is_none_or(|id| id == &entry.id)
            })
            .filter_map(|entry| entry.summary(include_command))
            .filter(|entry| request.status.is_none_or(|status| entry.status == status))
            .collect();
        drop(entries);
        runs.sort_by(|a, b| {
            b.started_at_ms
                .cmp(&a.started_at_ms)
                .then_with(|| a.background_id.cmp(&b.background_id))
        });
        let total = runs.len() as u32;
        Ok(ListBackgroundRunsResult {
            runs: runs
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect(),
            offset,
            limit,
            total,
        })
    }

    pub(super) async fn read(
        &self,
        request: ReadBackgroundOutputRequest,
    ) -> RuntimeResult<ReadBackgroundOutputResult> {
        let entry = self.visible(&request.session_id, &request.background_id)?;
        let output = {
            let state = entry.state.borrow();
            state.status().ok_or(RuntimeError::BackgroundRunNotFound)?;
            state
                .output()
                .cloned()
                .ok_or(RuntimeError::BackgroundOutputUnavailable)?
        };
        let page = output
            .read(BackgroundOutputRequest {
                cursor: request.cursor,
                offset_chars: request.offset_chars.unwrap_or(0),
                limit_chars: request.limit_chars.unwrap_or(2_000) as usize,
            })
            .await;
        let status = entry
            .state
            .borrow()
            .status()
            .ok_or(RuntimeError::BackgroundRunNotFound)?;
        let page = page.map_err(|error| match error {
            ShellToolError::BackgroundOutputClosed => RuntimeError::BackgroundRunNotFound,
            ShellToolError::InvalidInput { .. } => RuntimeError::InvalidRequest {
                reason: "invalid background output position or character limit",
            },
            _ => RuntimeError::BackgroundOutputUnavailable,
        })?;
        Ok(ReadBackgroundOutputResult {
            status,
            content: page.content,
            cursor: page.cursor,
            next_cursor: page.next_cursor,
            dropped_chars: page.dropped_chars,
        })
    }

    fn visible(&self, session_id: &SessionId, id: &str) -> RuntimeResult<Arc<Entry>> {
        self.find(session_id, id)
            .ok()
            .filter(|entry| entry.state.borrow().status().is_some())
            .ok_or(RuntimeError::BackgroundRunNotFound)
    }

    fn find(&self, session_id: &SessionId, id: &str) -> Result<Arc<Entry>, ShellToolError> {
        self.entries()
            .get(id)
            .filter(|entry| &entry.origin.session_id == session_id)
            .cloned()
            .ok_or_else(not_found)
    }

    pub(super) fn has_session(&self, session_id: &SessionId) -> bool {
        self.entries()
            .values()
            .any(|entry| &entry.origin.session_id == session_id)
    }

    pub(super) fn session_cleanup_unconfirmed(&self, id: &SessionId) -> bool {
        self.entries().values().any(|entry| {
            &entry.origin.session_id == id
                && matches!(*entry.state.borrow(), State::CleanupFailed(..))
        })
    }

    pub(super) fn request_session_stop(&self, id: &SessionId) {
        for entry in self
            .entries()
            .values()
            .filter(|entry| &entry.origin.session_id == id)
        {
            entry.stop.send_replace(true);
        }
    }

    pub(super) fn has_workspace(&self, workspace_id: &WorkspaceId) -> bool {
        self.entries()
            .values()
            .any(|entry| entry.workspace.as_ref() == Some(workspace_id))
    }

    pub(super) fn ids(&self) -> Vec<String> {
        self.entries().keys().cloned().collect()
    }

    pub(super) fn needs_cleanup(&self) -> bool {
        !self.entries().is_empty()
    }

    fn entries(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Entry>>> {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl Entry {
    fn summary(&self, include_command: bool) -> Option<BackgroundRunSummary> {
        let state = self.state.borrow();
        let output = state.output();
        Some(BackgroundRunSummary {
            background_id: self.id.clone(),
            run_id: self.origin.run_id.clone(),
            command_summary: self.command.chars().take(256).collect(),
            command: include_command.then(|| self.command.clone()),
            working_directory: self.working_directory.clone(),
            started_at_ms: self.started_at_ms,
            status: state.status()?,
            log_available: output.is_some_and(|output| output.is_available()),
            log_path: output.map(|output| output.path().to_string_lossy().into_owned()),
        })
    }

    fn update(&self, state: State, events: &ObservationCoordinator) {
        self.state.send_replace(state);
        if let Some(task) = &self.scheduled {
            task.background_changed.send_replace(());
        }
        let _ = events.send(RuntimeEvent::BackgroundRunChanged {
            session_id: self.origin.session_id.clone(),
            background_id: self.id.clone(),
        });
    }

    fn exit_result(&self, result: &BackgroundRunExit) -> StopBackgroundRunResult {
        StopBackgroundRunResult {
            background_id: self.id.clone(),
            exit_code: result.process.exit_code,
            timed_out: result.timed_out,
            output_error: result.process.output_error.clone(),
        }
    }

    async fn initial_result(
        &self,
        wait: Duration,
        cancellation: CancellationToken,
    ) -> Result<BackgroundShellStart, ShellToolError> {
        let mut state = self.state.subscribe();
        loop {
            let initial = state.borrow_and_update().clone();
            match initial {
                State::Starting => state.changed().await.map_err(|_| not_found())?,
                State::Running(_) => break,
                State::CleanupFailed(_, error) => return Err(error),
                State::Finished(result) => return result.map(BackgroundShellStart::Exited),
            };
        }

        let wait_until = Instant::now() + wait;
        loop {
            let snapshot = state.borrow_and_update().clone();
            match snapshot {
                State::Running(output) => {
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(ShellToolError::Cancelled),
                        result = state.changed() => { result.map_err(|_| not_found())?; continue; },
                        _ = sleep_until(wait_until) => {},
                    }
                    let preview = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(ShellToolError::Cancelled),
                        result = state.changed() => { result.map_err(|_| not_found())?; continue; },
                        preview = output.read(BackgroundOutputRequest {
                            cursor: None, offset_chars: 0, limit_chars: 2_000,
                        }) => preview,
                    };
                    // Adapter 在发布终态前先关闭文件。等监督器结算，避免把真实退出当成预览失败。
                    if matches!(preview, Err(ShellToolError::BackgroundOutputClosed)) {
                        tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => return Err(ShellToolError::Cancelled),
                            result = state.changed() => result.map_err(|_| not_found())?,
                        }
                        continue;
                    }
                    if !matches!(*state.borrow(), State::Running(_)) {
                        continue;
                    }
                    let (preview, output_error) = match preview {
                        Ok(preview) => (Some(preview), None),
                        Err(error) => (None, Some(error.to_string())),
                    };
                    return Ok(BackgroundShellStart::Running {
                        id: self.id.clone(),
                        log_path: output.path().to_owned(),
                        preview,
                        output_error,
                    });
                }
                State::Finished(result) => return result.map(BackgroundShellStart::Exited),
                State::CleanupFailed(_, error) => return Err(error),
                State::Starting => return Err(invalid("invalid background handoff")),
            }
        }
    }

    async fn stop(&self) -> Result<BackgroundRunExit, ShellToolError> {
        let mut state = self.state.subscribe();
        let current = state.borrow_and_update().clone();
        if let State::Finished(result) = current {
            return result;
        }
        if self.stop.send(true).is_err() {
            return match self.state.borrow().clone() {
                State::CleanupFailed(_, error) => Err(error),
                State::Finished(result) => result,
                _ => Err(not_found()),
            };
        }
        loop {
            state.changed().await.map_err(|_| not_found())?;
            match state.borrow_and_update().clone() {
                State::Finished(result) => return result,
                State::CleanupFailed(_, error) => return Err(error),
                State::Starting | State::Running(_) => {}
            }
        }
    }
}

fn invalid(message: &str) -> ShellToolError {
    ShellToolError::InvalidInput {
        message: message.into(),
    }
}

fn not_found() -> ShellToolError {
    invalid("background shell does not exist or has been cleaned up")
}
