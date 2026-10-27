//! Runtime 产品观察事件的单一序号分配与广播边界。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{RuntimeError, RuntimeResult};
use assistant_protocol::{
    ChildTaskEvent as ChildEvent, ConversationOwner, RuntimeEvent, RuntimeEventEnvelope, SessionId,
    WorkspaceId,
};
use tokio::sync::broadcast;

/// 组合读取真正消费的共享来源；与会话事件的作用域分开。
#[derive(Clone, Copy)]
pub(crate) enum SharedSnapshotSources {
    Configuration,
    Mcp,
    ConfigurationAndMcp,
}

struct WatchedScope {
    readers: usize,
    sequence: u64,
    durable_sequence: u64,
}

struct ObservationState {
    sequence: u64,
    application: u64,
    configuration: u64,
    mcp: u64,
    sessions: BTreeMap<SessionId, WatchedScope>,
    workspaces: BTreeMap<WorkspaceId, WatchedScope>,
}

enum ReadScope {
    Application,
    Shared(SharedSnapshotSources),
    Session {
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
        shared: SharedSnapshotSources,
        captures_live_runs: bool,
    },
}

/// 只在实际读取期间登记依赖；释放后回收 map，不为所有历史会话累积水位。
pub(crate) struct ObservationRead {
    inner: Arc<ObservationInner>,
    scope: ReadScope,
    initial: [u64; 4],
}

struct ObservationInner {
    state: Mutex<ObservationState>,
    sender: broadcast::Sender<RuntimeEventEnvelope>,
    legacy_sender: broadcast::Sender<RuntimeEvent>,
}

/// 所有 Runtime 可观察事件共享的短发布边界。
///
/// 序号分配和广播在同一个非异步临界区内完成，因此并发发布者在接收端看到的顺序与
/// `sequence` 完全一致。这里不持有业务状态，也不会跨 Store I/O、Provider 调用或审批等待。
#[derive(Clone)]
pub(crate) struct ObservationCoordinator {
    inner: Arc<ObservationInner>,
}

impl ObservationCoordinator {
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        // 裸事件流只服务既有嵌入式调用。聚合失效事件增加后给它保留兼容余量；正式 Host
        // 仍使用上面的精确产品容量，并在落后时通过 stream_gap 恢复。
        let legacy_capacity = capacity.saturating_mul(4).max(capacity);
        let (legacy_sender, _) = broadcast::channel(legacy_capacity);
        Self {
            inner: Arc::new(ObservationInner {
                state: Mutex::new(ObservationState {
                    sequence: 0,
                    application: 0,
                    configuration: 0,
                    mcp: 0,
                    sessions: BTreeMap::new(),
                    workspaces: BTreeMap::new(),
                }),
                sender,
                legacy_sender,
            }),
        }
    }

    /// 在当前 Runtime 实例内分配严格递增序号并发布事件。
    pub(crate) fn send(&self, event: RuntimeEvent) -> Result<usize, ()> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.publish_locked(&mut state, event)
    }

    /// 业务易失值的安装与对应事件在同一短发布边界内完成；闭包不得等待或再次发布。
    ///
    /// # Errors
    /// 返回安装闭包的错误；此时不发布事件。
    pub(crate) fn publish_transition<T>(
        &self,
        install: impl FnOnce() -> RuntimeResult<(T, Option<RuntimeEvent>)>,
    ) -> RuntimeResult<T> {
        let mut state =
            self.inner
                .state
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "observation publication",
                })?;
        let (value, event) = install()?;
        if let Some(event) = event {
            let _ = self.publish_locked(&mut state, event);
        }
        Ok(value)
    }

    /// 一次可靠提交产生的多个事件共享同一安装边界，并按调用方给定顺序发布。
    pub(crate) fn publish_batch_transition<T>(
        &self,
        install: impl FnOnce() -> RuntimeResult<(T, Vec<RuntimeEvent>)>,
    ) -> RuntimeResult<T> {
        let mut state =
            self.inner
                .state
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "observation publication",
                })?;
        let (value, events) = install()?;
        for event in events {
            let _ = self.publish_locked(&mut state, event);
        }
        Ok(value)
    }

    fn publish_locked(
        &self,
        state: &mut ObservationState,
        event: RuntimeEvent,
    ) -> Result<usize, ()> {
        state.sequence = state.sequence.saturating_add(1);
        state.observe(&event);
        let legacy_event = event.clone();
        let envelope = RuntimeEventEnvelope {
            sequence: state.sequence,
            emitted_at_ms: emitted_at_ms(),
            event,
        };
        let result = self.inner.sender.send(envelope).map_err(|_| ());
        // 裸事件订阅只保留给 Runtime crate 的既有嵌入式调用和测试；产品 Host 使用 envelope。
        let _ = self.inner.legacy_sender.send(legacy_event);
        result
    }

    pub(crate) fn read_application(&self) -> ObservationRead {
        self.begin_read(ReadScope::Application)
    }

    /// 可靠历史已由 owner gate 保护，只额外依赖当前用于工具投影的 MCP 目录。
    pub(crate) fn read_history(&self) -> ObservationRead {
        self.begin_read(ReadScope::Shared(SharedSnapshotSources::Mcp))
    }

    pub(crate) fn read_session(
        &self,
        session_id: &SessionId,
        workspace_id: Option<WorkspaceId>,
        shared: SharedSnapshotSources,
    ) -> ObservationRead {
        self.begin_read(ReadScope::Session {
            session_id: session_id.clone(),
            workspace_id,
            shared,
            captures_live_runs: false,
        })
    }

    /// 主会话读取在 capture 中复制最新 Run 流式状态，文字增量不使可靠正文读取重试。
    pub(crate) fn read_session_view(
        &self,
        session_id: &SessionId,
        workspace_id: Option<WorkspaceId>,
    ) -> ObservationRead {
        self.begin_read(ReadScope::Session {
            session_id: session_id.clone(),
            workspace_id,
            shared: SharedSnapshotSources::ConfigurationAndMcp,
            captures_live_runs: true,
        })
    }

    fn begin_read(&self, scope: ReadScope) -> ObservationRead {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sequence = state.sequence;
        if let ReadScope::Session {
            session_id,
            workspace_id,
            ..
        } = &scope
        {
            state
                .sessions
                .entry(session_id.clone())
                .or_insert(WatchedScope {
                    readers: 0,
                    sequence,
                    durable_sequence: sequence,
                })
                .readers += 1;
            if let Some(workspace_id) = workspace_id {
                state
                    .workspaces
                    .entry(workspace_id.clone())
                    .or_insert(WatchedScope {
                        readers: 0,
                        sequence,
                        durable_sequence: sequence,
                    })
                    .readers += 1;
            }
        }
        let initial = state.stamp(&scope);
        ObservationRead {
            inner: self.inner.clone(),
            scope,
            initial,
        }
    }

    /// 返回已经完成发布的最后一个事件序号。
    #[cfg(test)]
    pub(crate) fn sequence(&self) -> u64 {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sequence
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEventEnvelope> {
        self.inner.sender.subscribe()
    }

    pub(crate) fn subscribe_legacy(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.inner.legacy_sender.subscribe()
    }
}

impl ObservationState {
    fn stamp(&self, scope: &ReadScope) -> [u64; 4] {
        match scope {
            ReadScope::Application => [self.application, 0, 0, 0],
            ReadScope::Shared(shared) => [
                0,
                0,
                if matches!(
                    shared,
                    SharedSnapshotSources::Configuration
                        | SharedSnapshotSources::ConfigurationAndMcp
                ) {
                    self.configuration
                } else {
                    0
                },
                if matches!(
                    shared,
                    SharedSnapshotSources::Mcp | SharedSnapshotSources::ConfigurationAndMcp
                ) {
                    self.mcp
                } else {
                    0
                },
            ],
            ReadScope::Session {
                session_id,
                workspace_id,
                shared,
                captures_live_runs,
            } => [
                self.sessions.get(session_id).map_or(0, |scope| {
                    if *captures_live_runs {
                        scope.durable_sequence
                    } else {
                        scope.sequence
                    }
                }),
                workspace_id
                    .as_ref()
                    .and_then(|id| self.workspaces.get(id))
                    .map_or(0, |scope| scope.sequence),
                if matches!(
                    shared,
                    SharedSnapshotSources::Configuration
                        | SharedSnapshotSources::ConfigurationAndMcp
                ) {
                    self.configuration
                } else {
                    0
                },
                if matches!(
                    shared,
                    SharedSnapshotSources::Mcp | SharedSnapshotSources::ConfigurationAndMcp
                ) {
                    self.mcp
                } else {
                    0
                },
            ],
        }
    }

    fn observe(&mut self, event: &RuntimeEvent) {
        use RuntimeEvent::*;
        let (session_id, application) = match event {
            RuntimeShuttingDown => (None, true),
            ConfigChanged => {
                self.configuration = self.sequence;
                (None, true)
            }
            McpRegistryChanged => {
                self.mcp = self.sequence;
                (None, true)
            }
            WorkspaceChanged { workspace_id } => {
                if let Some(scope) = self.workspaces.get_mut(workspace_id) {
                    scope.sequence = self.sequence;
                }
                (None, true)
            }
            SessionCreated { session }
            | SessionVariantChanged { session }
            | SessionApprovalModeChanged { session } => (Some(&session.session_id), true),
            ApprovalRequested { approval } => (Some(&approval.session_id), true),
            FeedbackPublished { request } | FeedbackChanged { request } => {
                (Some(&request.session_id), true)
            }
            ConversationCommitted { owner, .. } | StepCommitted { owner, .. } => (
                Some(match owner {
                    ConversationOwner::MainSession { session_id }
                    | ConversationOwner::ChildTask { session_id, .. } => session_id,
                }),
                matches!(owner, ConversationOwner::MainSession { .. }),
            ),
            ChildTaskEvent {
                session_id, event, ..
            } => (
                Some(session_id),
                matches!(
                    event,
                    ChildEvent::Created { .. }
                        | ChildEvent::Started { .. }
                        | ChildEvent::Finished { .. }
                ),
            ),
            SessionChanged { session_id }
            | SessionDeleted { session_id }
            | SessionCompactionStarted { session_id, .. }
            | SessionCompactionFinished { session_id, .. }
            | SessionTitleGenerationStarted { session_id, .. }
            | SessionTitleGenerationFinished { session_id, .. }
            | QueueChanged { session_id, .. }
            | WorkPlanChanged { session_id, .. }
            | GoalChanged { session_id, .. }
            | ApprovalResolved { session_id, .. }
            | ApprovalCancelled { session_id, .. }
            | RunAccepted { session_id, .. }
            | RunStarted { session_id, .. }
            | RunCancelling { session_id, .. }
            | RunFinished { session_id, .. } => (Some(session_id), true),
            PermissionReloaded { session_id, .. }
            | StepStarted { session_id, .. }
            | ModelAttemptStarted { session_id, .. }
            | ModelAttemptFailed { session_id, .. }
            | ModelRetryScheduled { session_id, .. }
            | ModelStreamEstablished { session_id, .. }
            | TextDelta { session_id, .. }
            | ReasoningDelta { session_id, .. }
            | UsageUpdated { session_id, .. }
            | ToolProposed { session_id, .. }
            | ToolStarted { session_id, .. }
            | ToolOutput { session_id, .. }
            | ToolCompleted { session_id, .. }
            | GuardrailTriggered { session_id, .. } => (Some(session_id), false),
            PermissionChanged
            | SkillSettingsChanged { .. }
            | PluginOperationChanged { .. }
            | PluginCatalogChanged
            | CustomAgentCatalogChanged => (None, false),
        };
        if application {
            self.application = self.sequence;
        }
        if let Some(scope) = session_id.and_then(|id| self.sessions.get_mut(id)) {
            scope.sequence = self.sequence;
            if !matches!(
                event,
                RuntimeEvent::TextDelta { .. }
                    | RuntimeEvent::ReasoningDelta { .. }
                    | RuntimeEvent::ToolOutput { .. }
            ) {
                scope.durable_sequence = self.sequence;
            }
        }
    }
}

impl ObservationRead {
    /// 在与发布相同的边界检查依赖并复制实时字段，返回安全的全局水位；不相关输出不使其失效。
    ///
    /// # Errors
    /// 观察锁不可用或复制闭包失败时返回错误。
    pub(crate) fn capture<T>(
        &self,
        copy: impl FnOnce() -> RuntimeResult<T>,
    ) -> RuntimeResult<Option<(u64, T)>> {
        let state =
            self.inner
                .state
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "observation snapshot",
                })?;
        if state.stamp(&self.scope) != self.initial {
            return Ok(None);
        }
        Ok(Some((state.sequence, copy()?)))
    }
}

impl Drop for ObservationRead {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let ReadScope::Session {
            session_id,
            workspace_id,
            ..
        } = &self.scope
        {
            if let Some(scope) = state.sessions.get_mut(session_id) {
                scope.readers -= 1;
                if scope.readers == 0 {
                    state.sessions.remove(session_id);
                }
            }
            if let Some(id) = workspace_id
                && let Some(scope) = state.workspaces.get_mut(id)
            {
                scope.readers -= 1;
                if scope.readers == 0 {
                    state.workspaces.remove(id);
                }
            }
        }
    }
}

fn emitted_at_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_captures_streamed_value_at_latest_watermark_without_restarting_body_reads() {
        let events = ObservationCoordinator::new(8);
        let session = SessionId::new("streaming").unwrap();
        let read = events.read_session_view(&session, None);
        let strict =
            events.read_session(&session, None, SharedSnapshotSources::ConfigurationAndMcp);
        let text = Mutex::new(String::new());
        events
            .publish_transition(|| {
                *text.lock().unwrap() = "latest".into();
                Ok((
                    (),
                    Some(RuntimeEvent::TextDelta {
                        session_id: session.clone(),
                        run_id: assistant_protocol::RunId::new("run").unwrap(),
                        step: 1,
                        part_id: assistant_protocol::PartId::new("part").unwrap(),
                        delta: "latest".into(),
                    }),
                ))
            })
            .unwrap();
        assert_eq!(
            read.capture(|| Ok(text.lock().unwrap().clone())).unwrap(),
            Some((1, "latest".into()))
        );
        assert!(strict.capture(|| Ok(())).unwrap().is_none());
        let _ = events.send(RuntimeEvent::ConversationCommitted {
            owner: ConversationOwner::MainSession {
                session_id: session,
            },
            generation: 1,
        });
        assert!(
            read.capture(|| Ok(())).unwrap().is_none(),
            "reliable commits still invalidate the view"
        );
    }

    #[test]
    fn scoped_reads_track_only_live_dependencies_and_release_their_catalogue() {
        let events = ObservationCoordinator::new(8);
        let first = SessionId::new("first").unwrap();
        let second = SessionId::new("second").unwrap();
        let workspace = WorkspaceId::new("workspace").unwrap();
        let app = events.read_application();
        let read = events.read_session(
            &second,
            Some(workspace.clone()),
            SharedSnapshotSources::ConfigurationAndMcp,
        );
        let history = events.read_history();
        let _ = events.send(RuntimeEvent::TextDelta {
            session_id: first,
            run_id: assistant_protocol::RunId::new("r").unwrap(),
            step: 1,
            part_id: assistant_protocol::PartId::new("p").unwrap(),
            delta: "noise".into(),
        });
        assert!(app.capture(|| Ok(())).unwrap().is_some());
        assert!(read.capture(|| Ok(())).unwrap().is_some());
        let _ = events.send(RuntimeEvent::ConfigChanged);
        assert!(read.capture(|| Ok(())).unwrap().is_none());
        assert!(app.capture(|| Ok(())).unwrap().is_none());
        assert!(history.capture(|| Ok(())).unwrap().is_some());
        let _ = events.send(RuntimeEvent::McpRegistryChanged);
        assert!(history.capture(|| Ok(())).unwrap().is_none());
        let nested = events.read_session(
            &second,
            Some(workspace.clone()),
            SharedSnapshotSources::Configuration,
        );
        drop(read);
        assert_eq!(events.inner.state.lock().unwrap().sessions.len(), 1);
        let _ = events.send(RuntimeEvent::WorkspaceChanged {
            workspace_id: WorkspaceId::new("other").unwrap(),
        });
        assert!(nested.capture(|| Ok(())).unwrap().is_some());
        let _ = events.send(RuntimeEvent::WorkspaceChanged {
            workspace_id: workspace,
        });
        assert!(nested.capture(|| Ok(())).unwrap().is_none());
        drop(nested);
        let state = events.inner.state.lock().unwrap();
        assert!(state.sessions.is_empty());
        assert!(state.workspaces.is_empty());
    }

    #[test]
    fn transition_values_are_captured_with_their_published_watermark() {
        let events = ObservationCoordinator::new(8);
        let value = std::sync::atomic::AtomicU64::new(0);
        let before = events.read_application();
        events
            .publish_transition(|| {
                value.store(1, std::sync::atomic::Ordering::SeqCst);
                Ok(((), Some(RuntimeEvent::RuntimeShuttingDown)))
            })
            .unwrap();
        assert!(
            before
                .capture(|| Ok(value.load(std::sync::atomic::Ordering::SeqCst)))
                .unwrap()
                .is_none()
        );
        let after = events.read_application();
        assert_eq!(
            after
                .capture(|| Ok(value.load(std::sync::atomic::Ordering::SeqCst)))
                .unwrap(),
            Some((1, 1))
        );
    }

    #[tokio::test]
    async fn concurrent_publish_order_matches_sequence_order() {
        let coordinator = ObservationCoordinator::new(8);
        let mut receiver = coordinator.subscribe();
        let left = coordinator.clone();
        let right = coordinator.clone();

        let (left_result, right_result) = tokio::join!(
            tokio::spawn(async move { left.send(RuntimeEvent::RuntimeShuttingDown) }),
            tokio::spawn(async move { right.send(RuntimeEvent::RuntimeShuttingDown) }),
        );
        left_result.expect("left publisher").expect("left event");
        right_result.expect("right publisher").expect("right event");

        assert_eq!(receiver.recv().await.expect("first").sequence, 1);
        assert_eq!(receiver.recv().await.expect("second").sequence, 2);
        assert_eq!(coordinator.sequence(), 2);
    }

    #[test]
    fn snapshot_cannot_accept_half_a_reliable_child_terminal_publication() {
        let events = ObservationCoordinator::new(8);
        let mut stream = events.subscribe();
        let value = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (installed, entered) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let writer_events = events.clone();
        let written = value.clone();
        let writer = std::thread::spawn(move || {
            writer_events
                .publish_batch_transition(|| {
                    written.store(true, std::sync::atomic::Ordering::SeqCst);
                    installed.send(()).unwrap();
                    resume.recv().unwrap();
                    let session_id = SessionId::new("parent").unwrap();
                    let child_task_id = assistant_protocol::ChildTaskId::new("child").unwrap();
                    Ok((
                        (),
                        vec![
                            RuntimeEvent::ConversationCommitted {
                                owner: ConversationOwner::ChildTask {
                                    session_id: session_id.clone(),
                                    child_task_id: child_task_id.clone(),
                                },
                                generation: 1,
                            },
                            RuntimeEvent::ChildTaskEvent {
                                session_id,
                                child_task_id,
                                parent_run_id: assistant_protocol::RunId::new("run").unwrap(),
                                event: ChildEvent::Finished {
                                    status: assistant_protocol::ChildTaskStatus::Completed,
                                    error: None,
                                    finished_at_ms: Some(1),
                                },
                            },
                        ],
                    ))
                })
                .unwrap();
        });
        entered.recv().unwrap();
        // 即使 registry 已安装，读取也不能拿到安装前或批次中途的发布水位。
        assert!(events.inner.state.try_lock().is_err());
        release.send(()).unwrap();
        let snapshot = events
            .read_application()
            .capture(|| Ok(value.load(std::sync::atomic::Ordering::SeqCst)))
            .unwrap();
        writer.join().unwrap();
        assert_eq!(snapshot, Some((2, true)));
        let committed = stream.try_recv().unwrap();
        let terminal = stream.try_recv().unwrap();
        assert!(matches!(
            committed.event,
            RuntimeEvent::ConversationCommitted { .. }
        ));
        assert!(matches!(
            terminal.event,
            RuntimeEvent::ChildTaskEvent {
                event: ChildEvent::Finished { .. },
                ..
            }
        ));
        assert_eq!((committed.sequence, terminal.sequence), (1, 2));
    }
}
