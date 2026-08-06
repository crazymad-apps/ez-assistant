//! AssistantRuntime 生命周期门禁和 Session Registry。

use std::{
    collections::{BTreeMap, btree_map::Entry},
    panic::AssertUnwindSafe,
    sync::{Arc, RwLock},
};

use agent_core::{AgentExecution, ExecutionContext, ExecutionInput, ToolAuthorizer};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, UserMessage, UserPart,
};
use assistant_protocol::{
    CancelRunRequest, CancelRunResult, CreateSessionRequest, CreateSessionResult, GetRunRequest,
    GetRunResult, GetSessionRequest, GetSessionResult, ListSessionsRequest, ListSessionsResult,
    RunId, RuntimeEvent, RuntimeLifecycle, SessionId, SessionSummary, ShutdownRuntimeRequest,
    ShutdownRuntimeResult, StartRunRequest, StartRunResult,
};
use tokio::sync::broadcast;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    RuntimeConfig, RuntimeError, RuntimeResult, SessionAgentFactory, id,
    run::{ActiveRun, RunRecord, RuntimeRecorder, settle_run, supervise_run},
    session::{SessionController, SessionState},
};

const ID_GENERATION_ATTEMPTS: usize = 16;

/// 应用业务 Runtime 的进程内权威入口。
///
/// Drop 只释放内存，不冒充异步关闭；宿主退出前必须显式调用 [`Self::shutdown`]。
pub struct AssistantRuntime {
    config: RuntimeConfig,
    lifecycle: RwLock<RuntimeLifecycle>,
    sessions: RwLock<BTreeMap<SessionId, Arc<SessionController>>>,
    factory: Arc<dyn SessionAgentFactory>,
    default_authorizer: Arc<dyn ToolAuthorizer>,
    event_sender: broadcast::Sender<RuntimeEvent>,
    root_cancellation: CancellationToken,
    tasks: TaskTracker,
}

impl AssistantRuntime {
    /// 使用显式配置、Session Agent 工厂和默认授权闸创建 Running Runtime。
    pub fn new(
        config: RuntimeConfig,
        factory: Arc<dyn SessionAgentFactory>,
        default_authorizer: Arc<dyn ToolAuthorizer>,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(config.event_capacity.get());
        Self {
            config,
            lifecycle: RwLock::new(RuntimeLifecycle::Running),
            sessions: RwLock::new(BTreeMap::new()),
            factory,
            default_authorizer,
            event_sender,
            root_cancellation: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }
    }

    /// 返回构造时已经校验的 Runtime 配置。
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// 查询当前 Runtime 生命周期。
    pub fn lifecycle(&self) -> RuntimeResult<RuntimeLifecycle> {
        self.lifecycle
            .read()
            .map(|lifecycle| *lifecycle)
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "runtime lifecycle",
            })
    }

    /// 订阅 Runtime 的有界实时事件流；落后的订阅者必须通过快照重新对齐。
    pub fn subscribe_events(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.event_sender.subscribe()
    }

    /// 创建一个拥有独立冻结 Agent 和空 Conversation 的 Session。
    pub fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<CreateSessionResult> {
        self.ensure_running()?;

        let title = request.title.unwrap_or_else(|| "New Session".to_owned());
        let agent = self
            .factory
            .create_agent()
            .map_err(|source| RuntimeError::AgentBuildFailed { source })?;

        let lifecycle = self.read_lifecycle()?;
        if *lifecycle != RuntimeLifecycle::Running {
            return Err(RuntimeError::RuntimeNotRunning {
                lifecycle: *lifecycle,
            });
        }
        let mut sessions =
            self.sessions
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "session registry",
                })?;
        let session_id = allocate_session_id(&sessions)?;
        let session = Arc::new(SessionController::new(session_id.clone(), title, agent));
        match sessions.entry(session_id) {
            Entry::Vacant(entry) => {
                entry.insert(session.clone());
            }
            Entry::Occupied(_) => {
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "session id collision",
                });
            }
        }
        drop(sessions);
        drop(lifecycle);

        let summary = session.summary()?;
        self.publish(RuntimeEvent::SessionCreated {
            session: summary.clone(),
        });
        Ok(CreateSessionResult { session: summary })
    }

    /// 按 SessionId 的确定性顺序列出当前进程内 Session。
    pub fn list_sessions(
        &self,
        _request: ListSessionsRequest,
    ) -> RuntimeResult<ListSessionsResult> {
        let sessions: Vec<_> = self
            .sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .values()
            .cloned()
            .collect();
        let summaries = sessions
            .into_iter()
            .map(|session| session.summary())
            .collect::<RuntimeResult<Vec<SessionSummary>>>()?;
        Ok(ListSessionsResult {
            sessions: summaries,
        })
    }

    /// 查询一个 Session 的稳定摘要。
    pub fn get_session(&self, request: GetSessionRequest) -> RuntimeResult<GetSessionResult> {
        Ok(GetSessionResult {
            session: self.session(&request.session_id)?.summary()?,
        })
    }

    /// 查询一个 Session 当前已经完整提交的规范 Conversation。
    ///
    /// 该返回值属于 Runtime library 能力，不进入 `assistant-protocol` 公共 DTO；Host 的完整
    /// Conversation 验证命令保持私有。
    pub fn conversation_snapshot(
        &self,
        session_id: &SessionId,
    ) -> RuntimeResult<ConversationSnapshot> {
        self.session(session_id)?.conversation_snapshot()
    }

    /// 向空闲 Session 原子追加用户消息并启动一次 AgentExecution。
    ///
    /// 调用线程必须位于可执行 `tokio::spawn` 的 Tokio Runtime 中。
    pub fn start_run(&self, request: StartRunRequest) -> RuntimeResult<StartRunResult> {
        // 读门禁一直持有到 supervisor 已登记进 TaskTracker，确保 shutdown 不会漏掉新任务。
        let lifecycle = self.read_lifecycle()?;
        if *lifecycle != RuntimeLifecycle::Running {
            return Err(RuntimeError::RuntimeNotRunning {
                lifecycle: *lifecycle,
            });
        }
        if request.message.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "message must not be blank",
            });
        }

        let session = self.session(&request.session_id)?;
        let user_message = create_user_message(request.message)?;
        let cancellation = self.root_cancellation.child_token();
        let (run_id, accepted, input) = {
            let mut state = session.lock_state()?;
            ensure_session_can_start(&state, session.id())?;
            let run_id = allocate_run_id(&state)?;
            state
                .journal
                .append_completed(ConversationMessage::User(user_message))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "user message commit",
                })?;
            let input = ExecutionInput {
                conversation: state.journal.snapshot(),
            };
            let record = RunRecord::accepted(run_id.clone(), session.id().clone());
            let accepted = record.snapshot();
            state.runs.insert(run_id.clone(), record);
            state.active_run = Some(ActiveRun {
                run_id: run_id.clone(),
                cancellation: cancellation.clone(),
            });
            (run_id, accepted, input)
        };

        self.publish(RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: run_id.clone(),
        });

        let recorder = Arc::new(RuntimeRecorder::new(session.clone(), run_id.clone()));
        let execution = std::panic::catch_unwind(AssertUnwindSafe(|| {
            session.agent().start(
                input,
                ExecutionContext {
                    cancellation,
                    recorder,
                    authorizer: self.default_authorizer.clone(),
                },
            )
        }));
        let AgentExecution {
            events,
            completion,
            control: _,
        } = match execution {
            Ok(execution) => execution,
            Err(_) => {
                if let Ok(snapshot) = settle_run(&session, &run_id, None) {
                    self.publish(run_finished_event(snapshot));
                }
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "agent execution start",
                });
            }
        };

        self.tasks.spawn(supervise_run(
            session,
            run_id,
            events,
            completion,
            self.event_sender.clone(),
        ));
        drop(lifecycle);
        Ok(StartRunResult { run: accepted })
    }

    /// 查询指定 Session 中的 Runtime Run 快照。
    pub fn get_run(&self, request: GetRunRequest) -> RuntimeResult<GetRunResult> {
        Ok(GetRunResult {
            run: self
                .session(&request.session_id)?
                .run_snapshot(&request.run_id)?,
        })
    }

    /// 请求取消一个活动 Run；终态 Run 重复取消会原样返回当前快照。
    pub fn cancel_run(&self, request: CancelRunRequest) -> RuntimeResult<CancelRunResult> {
        let session = self.session(&request.session_id)?;
        let (snapshot, cancellation) =
            {
                let mut state = session.lock_state()?;
                let record = state.runs.get_mut(&request.run_id).ok_or_else(|| {
                    RuntimeError::RunNotFound {
                        session_id: request.session_id.clone(),
                        run_id: request.run_id.clone(),
                    }
                })?;
                if record.snapshot().status.is_terminal() {
                    return Ok(CancelRunResult {
                        run: record.snapshot(),
                    });
                }
                let first_request = record.mark_cancelling();
                let snapshot = record.snapshot();
                let cancellation = state
                    .active_run
                    .as_ref()
                    .filter(|active| active.run_id == request.run_id)
                    .map(|active| active.cancellation.clone())
                    .ok_or(RuntimeError::InternalStateUnavailable {
                        component: "active run cancellation",
                    })?;
                if first_request {
                    self.publish(RuntimeEvent::RunCancelling {
                        session_id: request.session_id.clone(),
                        run_id: request.run_id.clone(),
                    });
                }
                (snapshot, cancellation)
            };

        cancellation.cancel();
        Ok(CancelRunResult { run: snapshot })
    }

    /// 拒绝新工作、取消活动 Run，并等待所有已登记 supervisor 完成结算。
    pub async fn shutdown(
        &self,
        _request: ShutdownRuntimeRequest,
    ) -> RuntimeResult<ShutdownRuntimeResult> {
        let cancellations = {
            let mut lifecycle =
                self.lifecycle
                    .write()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "runtime lifecycle",
                    })?;
            if *lifecycle == RuntimeLifecycle::Stopped {
                return Ok(ShutdownRuntimeResult {
                    lifecycle: RuntimeLifecycle::Stopped,
                });
            }
            let first_transition = *lifecycle == RuntimeLifecycle::Running;
            *lifecycle = RuntimeLifecycle::ShuttingDown;
            self.tasks.close();
            if first_transition {
                self.publish(RuntimeEvent::RuntimeShuttingDown);
            }

            let sessions: Vec<_> = self
                .sessions
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "session registry",
                })?
                .values()
                .cloned()
                .collect();
            let mut cancellations = Vec::new();
            for session in sessions {
                let mut state = session.lock_state()?;
                let Some((run_id, cancellation)) = state
                    .active_run
                    .as_ref()
                    .map(|active| (active.run_id.clone(), active.cancellation.clone()))
                else {
                    continue;
                };
                let first_request = state
                    .runs
                    .get_mut(&run_id)
                    .ok_or(RuntimeError::InternalStateUnavailable {
                        component: "active run record",
                    })?
                    .mark_cancelling();
                if first_request {
                    self.publish(RuntimeEvent::RunCancelling {
                        session_id: session.id().clone(),
                        run_id: run_id.clone(),
                    });
                }
                cancellations.push(cancellation);
            }
            cancellations
        };

        for cancellation in cancellations {
            cancellation.cancel();
        }
        self.root_cancellation.cancel();
        self.tasks.wait().await;

        *self
            .lifecycle
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "runtime lifecycle",
            })? = RuntimeLifecycle::Stopped;
        Ok(ShutdownRuntimeResult {
            lifecycle: RuntimeLifecycle::Stopped,
        })
    }

    fn ensure_running(&self) -> RuntimeResult<()> {
        let lifecycle = self.lifecycle()?;
        if lifecycle == RuntimeLifecycle::Running {
            Ok(())
        } else {
            Err(RuntimeError::RuntimeNotRunning { lifecycle })
        }
    }

    fn read_lifecycle(&self) -> RuntimeResult<std::sync::RwLockReadGuard<'_, RuntimeLifecycle>> {
        self.lifecycle
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "runtime lifecycle",
            })
    }

    fn session(&self, session_id: &SessionId) -> RuntimeResult<Arc<SessionController>> {
        self.sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .get(session_id)
            .cloned()
            .ok_or_else(|| RuntimeError::SessionNotFound {
                session_id: session_id.clone(),
            })
    }

    fn publish(&self, event: RuntimeEvent) {
        let _ = self.event_sender.send(event);
    }

    #[cfg(test)]
    fn set_lifecycle(&self, lifecycle: RuntimeLifecycle) {
        *self.lifecycle.write().expect("lifecycle lock") = lifecycle;
    }

    #[cfg(test)]
    fn session_for_test(&self, session_id: &SessionId) -> Arc<SessionController> {
        self.session(session_id).expect("session exists")
    }
}

fn run_finished_event(snapshot: assistant_protocol::RunSnapshot) -> RuntimeEvent {
    RuntimeEvent::RunFinished {
        session_id: snapshot.session_id,
        run_id: snapshot.run_id,
        status: snapshot.status,
        error: snapshot.error,
    }
}

fn allocate_session_id(
    sessions: &BTreeMap<SessionId, Arc<SessionController>>,
) -> RuntimeResult<SessionId> {
    for _ in 0..ID_GENERATION_ATTEMPTS {
        let value = id::generate("s").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id random source",
        })?;
        let id = SessionId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id generator",
        })?;
        if !sessions.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "session id collision",
    })
}

fn allocate_run_id(state: &SessionState) -> RuntimeResult<RunId> {
    for _ in 0..ID_GENERATION_ATTEMPTS {
        let value = id::generate("r").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "run id random source",
        })?;
        let id = RunId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "run id generator",
        })?;
        if !state.runs.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "run id collision",
    })
}

fn create_user_message(text: String) -> RuntimeResult<UserMessage> {
    let message_id = id::generate("m")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "message id random source",
        })
        .and_then(|value| {
            MessageId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "message id generator",
            })
        })?;
    let part_id = id::generate("p")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "part id random source",
        })
        .and_then(|value| {
            PartId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "part id generator",
            })
        })?;
    Ok(UserMessage {
        id: message_id,
        parts: vec![UserPart::Text(TextPart { id: part_id, text })],
    })
}

fn ensure_session_can_start(state: &SessionState, session_id: &SessionId) -> RuntimeResult<()> {
    if state.is_faulted {
        return Err(RuntimeError::SessionFaulted {
            session_id: session_id.clone(),
        });
    }
    if state.active_run.is_some() || state.journal.has_pending() {
        return Err(RuntimeError::SessionBusy {
            session_id: session_id.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelService,
        ModelStreamFuture,
    };
    use agent_sdk::{
        Agent, AgentBuilder, AllowAllAuthorizer, ContextWindowEvaluator, SystemPromptSnapshot,
    };
    use agent_testkit::{
        ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events,
    };
    use agent_tools::{
        ToolOutputChannel as AgentToolOutputChannel, ToolOutputChunk, ToolRegistry, ToolSetSnapshot,
    };
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, ModelIdentity, ProviderId, ToolCall,
        ToolCallId, ToolName,
    };
    use serde_json::json;
    use tokio::sync::{Barrier, Notify, broadcast::error::RecvError};

    use super::*;
    use crate::AgentFactoryError;

    struct CountingFactory {
        created: AtomicUsize,
    }

    impl CountingFactory {
        fn new() -> Self {
            Self {
                created: AtomicUsize::new(0),
            }
        }

        fn created(&self) -> usize {
            self.created.load(Ordering::Relaxed)
        }
    }

    impl SessionAgentFactory for CountingFactory {
        fn create_agent(&self) -> Result<Agent, AgentFactoryError> {
            let sequence = self.created.fetch_add(1, Ordering::Relaxed) + 1;
            let model = Arc::new(ScriptedModelService::new(
                ModelCapabilities {
                    reasoning: false,
                    tool_calls: false,
                    streaming: true,
                },
                8_192,
                [],
            ));
            AgentBuilder::new(
                model,
                SystemPromptSnapshot::new(vec![format!("Session agent {sequence}")]),
                Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
            )
            .build()
            .map_err(|source| AgentFactoryError::with_source("agent build failed", source))
        }
    }

    struct FailingFactory;

    impl SessionAgentFactory for FailingFactory {
        fn create_agent(&self) -> Result<Agent, AgentFactoryError> {
            Err(AgentFactoryError::new("fixture failure"))
        }
    }

    struct StaticFactory {
        model: Arc<dyn ModelService>,
        tools: ToolSetSnapshot,
    }

    impl StaticFactory {
        fn new(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> Self {
            Self { model, tools }
        }
    }

    impl SessionAgentFactory for StaticFactory {
        fn create_agent(&self) -> Result<Agent, AgentFactoryError> {
            AgentBuilder::new(
                self.model.clone(),
                SystemPromptSnapshot::new(vec!["Runtime test agent".to_owned()]),
                Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
            )
            .tools(self.tools.clone())
            .build()
            .map_err(|source| AgentFactoryError::with_source("agent build failed", source))
        }
    }

    struct PanicModel {
        capabilities: ModelCapabilities,
    }

    impl ModelService for PanicModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            8_192
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            panic!("intentional model panic")
        }
    }

    fn runtime(factory: Arc<dyn SessionAgentFactory>) -> AssistantRuntime {
        runtime_with_capacity(factory, 32)
    }

    fn runtime_with_capacity(
        factory: Arc<dyn SessionAgentFactory>,
        event_capacity: usize,
    ) -> AssistantRuntime {
        AssistantRuntime::new(
            RuntimeConfig::new(NonZeroUsize::new(event_capacity).expect("non-zero")),
            factory,
            Arc::new(AllowAllAuthorizer),
        )
    }

    fn model_capabilities(has_tools: bool) -> ModelCapabilities {
        ModelCapabilities {
            reasoning: false,
            tool_calls: has_tools,
            streaming: true,
        }
    }

    fn assistant_text(message_id: &str, text: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(message_id).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new(format!("{message_id}-text")).expect("part id"),
                text: text.to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn assistant_tool_call(message_id: &str, tool_name: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(message_id).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("call-1").expect("tool call id"),
                name: ToolName::new(tool_name).expect("tool name"),
                arguments: json!({"value": "hello"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn hanging_runtime(
        hanging_count: usize,
        final_text: Option<&str>,
        entered: Arc<Notify>,
        cleanup: Arc<Notify>,
    ) -> AssistantRuntime {
        let mut registry = ToolRegistry::new();
        registry
            .register(
                ScriptedTool::hanging("slow_tool", OrderLog::new())
                    .with_entered_signal(entered)
                    .with_cleanup_signal(cleanup),
            )
            .expect("register hanging tool");
        let tool_message = assistant_tool_call("assistant-tools", "slow_tool");
        let mut scripts = (0..hanging_count)
            .map(|_| ModelScript::Events(message_events(&tool_message)))
            .collect::<Vec<_>>();
        if let Some(text) = final_text {
            scripts.push(ModelScript::Events(message_events(&assistant_text(
                "assistant-final",
                text,
            ))));
        }
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8_192,
            scripts,
        ));
        runtime(Arc::new(StaticFactory::new(model, registry.snapshot())))
    }

    async fn wait_for_terminal(
        runtime: &AssistantRuntime,
        session_id: &SessionId,
        run_id: &RunId,
    ) -> assistant_protocol::RunSnapshot {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = runtime
                    .get_run(GetRunRequest {
                        session_id: session_id.clone(),
                        run_id: run_id.clone(),
                    })
                    .expect("run query")
                    .run;
                if snapshot.status.is_terminal() {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("run reaches terminal state")
    }

    #[test]
    fn creates_one_distinct_agent_and_empty_conversation_per_session() {
        let factory = Arc::new(CountingFactory::new());
        let runtime = runtime(factory.clone());
        let first = runtime
            .create_session(CreateSessionRequest {
                title: Some("First".to_owned()),
            })
            .expect("first session");
        let second = runtime
            .create_session(CreateSessionRequest::default())
            .expect("second session");

        assert_eq!(factory.created(), 2);
        assert_ne!(first.session.session_id, second.session.session_id);
        assert!(first.session.session_id.as_str().starts_with("s_"));
        assert_eq!(first.session.session_id.as_str().len(), 14);
        assert_eq!(first.session.title, "First");
        assert_eq!(second.session.title, "New Session");
        assert_eq!(first.session.message_count, 0);
        assert_eq!(second.session.message_count, 0);
        assert!(first.session.active_run_id.is_none());
        assert!(second.session.active_run_id.is_none());
        assert!(
            runtime
                .conversation_snapshot(&first.session.session_id)
                .expect("first conversation")
                .messages
                .is_empty()
        );
        assert!(
            runtime
                .conversation_snapshot(&second.session.session_id)
                .expect("second conversation")
                .messages
                .is_empty()
        );

        let first_agent = runtime.session_for_test(&first.session.session_id).agent();
        let second_agent = runtime.session_for_test(&second.session.session_id).agent();
        assert!(!Arc::ptr_eq(&first_agent, &second_agent));
        assert_ne!(first_agent.system_prompt(), second_agent.system_prompt());
    }

    #[test]
    fn list_and_get_are_deterministic_and_unknown_session_is_structured() {
        let runtime = runtime(Arc::new(CountingFactory::new()));
        let first = runtime
            .create_session(CreateSessionRequest::default())
            .expect("first session");
        let second = runtime
            .create_session(CreateSessionRequest::default())
            .expect("second session");

        let listed = runtime
            .list_sessions(ListSessionsRequest::default())
            .expect("list sessions");
        let listed_ids = listed
            .sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        let mut expected_ids = vec![
            first.session.session_id.clone(),
            second.session.session_id.clone(),
        ];
        expected_ids.sort();
        assert_eq!(listed_ids, expected_ids);
        assert_eq!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: second.session.session_id.clone(),
                })
                .expect("get session")
                .session,
            second.session
        );

        let missing = SessionId::new("missing").expect("session id");
        assert!(matches!(
            runtime.get_session(GetSessionRequest {
                session_id: missing.clone()
            }),
            Err(RuntimeError::SessionNotFound { session_id }) if session_id == missing
        ));
    }

    #[test]
    fn factory_failure_does_not_insert_a_partial_session() {
        let runtime = runtime(Arc::new(FailingFactory));
        assert!(matches!(
            runtime.create_session(CreateSessionRequest::default()),
            Err(RuntimeError::AgentBuildFailed { .. })
        ));
        assert!(
            runtime
                .list_sessions(ListSessionsRequest::default())
                .expect("list sessions")
                .sessions
                .is_empty()
        );
    }

    #[test]
    fn non_running_lifecycle_rejects_new_sessions_but_queries_remain_available() {
        let runtime = runtime(Arc::new(CountingFactory::new()));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");

        for lifecycle in [RuntimeLifecycle::ShuttingDown, RuntimeLifecycle::Stopped] {
            runtime.set_lifecycle(lifecycle);
            assert!(matches!(
                runtime.create_session(CreateSessionRequest::default()),
                Err(RuntimeError::RuntimeNotRunning { lifecycle: actual }) if actual == lifecycle
            ));
            assert_eq!(runtime.lifecycle().expect("lifecycle"), lifecycle);
            assert!(matches!(
                runtime.start_run(StartRunRequest {
                    session_id: session.session.session_id.clone(),
                    message: "must not start".to_owned(),
                }),
                Err(RuntimeError::RuntimeNotRunning { lifecycle: actual }) if actual == lifecycle
            ));
            assert_eq!(
                runtime
                    .get_session(GetSessionRequest {
                        session_id: session.session.session_id.clone()
                    })
                    .expect("query remains available")
                    .session,
                session.session
            );
        }
    }

    #[test]
    fn runtime_is_send_and_sync_without_session_tasks() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AssistantRuntime>();
    }

    #[tokio::test]
    async fn completed_run_commits_user_before_model_and_final_assistant_once() {
        let final_message = assistant_text("assistant-final", "done");
        let model = Arc::new(ScriptedModelService::completing(
            model_capabilities(false),
            8_192,
            final_message.clone(),
        ));
        let runtime = runtime(Arc::new(StaticFactory::new(
            model.clone(),
            ToolSetSnapshot::default(),
        )));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");

        let started = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "hello".to_owned(),
            })
            .expect("run accepted");
        assert_eq!(started.run.status, assistant_protocol::RunStatus::Accepted);
        assert!(started.run.run_id.as_str().starts_with("r_"));

        let terminal =
            wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
        assert_eq!(terminal.text, "done");
        assert!(terminal.error.is_none());

        let requests = model.take_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            requests[0].conversation.messages.as_slice(),
            [ConversationMessage::User(_)]
        ));
        let conversation = runtime
            .conversation_snapshot(&session.session.session_id)
            .expect("conversation");
        assert_eq!(conversation.messages.len(), 2);
        assert!(matches!(
            conversation.messages[0],
            ConversationMessage::User(_)
        ));
        assert_eq!(
            conversation.messages[1],
            ConversationMessage::Assistant(final_message)
        );
        assert_eq!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: session.session.session_id.clone(),
                })
                .expect("session query")
                .session
                .active_run_id,
            None
        );
    }

    #[tokio::test]
    async fn slow_or_dropped_event_subscribers_never_block_run_completion() {
        let final_message = assistant_text("assistant-final", "done");
        let model = Arc::new(ScriptedModelService::completing(
            model_capabilities(false),
            8_192,
            final_message,
        ));
        let runtime = runtime_with_capacity(
            Arc::new(StaticFactory::new(model, ToolSetSnapshot::default())),
            1,
        );
        let mut lagging = runtime.subscribe_events();
        let dropped = runtime.subscribe_events();
        drop(dropped);
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        let started = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "hello".to_owned(),
            })
            .expect("run accepted");

        let terminal =
            wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
        assert!(matches!(lagging.recv().await, Err(RecvError::Lagged(_))));
    }

    #[tokio::test]
    async fn successful_tool_exchange_is_committed_before_the_next_model_step() {
        let log = OrderLog::new();
        let mut registry = ToolRegistry::new();
        registry
            .register(
                ScriptedTool::succeed("echo_tool", json!({"echo": "hello"}), log)
                    .with_output_chunks(vec![
                        ToolOutputChunk {
                            channel: AgentToolOutputChannel::Stdout,
                            delta: "hello".to_owned(),
                        },
                        ToolOutputChunk {
                            channel: AgentToolOutputChannel::Stderr,
                            delta: "warning".to_owned(),
                        },
                    ]),
            )
            .expect("register tool");
        let tool_message = assistant_tool_call("assistant-tools", "echo_tool");
        let final_message = assistant_text("assistant-final", "tool finished");
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8_192,
            [
                ModelScript::Events(message_events(&tool_message)),
                ModelScript::Events(message_events(&final_message)),
            ],
        ));
        let runtime = runtime(Arc::new(StaticFactory::new(
            model.clone(),
            registry.snapshot(),
        )));
        let mut events = runtime.subscribe_events();
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        let started = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "use echo".to_owned(),
            })
            .expect("run accepted");

        let terminal =
            wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
        assert_eq!(terminal.text, "tool finished");
        let conversation = runtime
            .conversation_snapshot(&session.session.session_id)
            .expect("conversation");
        assert_eq!(conversation.messages.len(), 4);
        conversation
            .validate_tool_exchange_pairs()
            .expect("tool exchange remains canonical");
        assert_eq!(model.take_requests().len(), 2);

        let observed = tokio::time::timeout(Duration::from_secs(1), async {
            let mut observed = Vec::new();
            loop {
                let event = events
                    .recv()
                    .await
                    .expect("event remains in bounded buffer");
                let finished = matches!(
                    &event,
                    RuntimeEvent::RunFinished { run_id, .. } if run_id == &started.run.run_id
                );
                observed.push(event);
                if finished {
                    return observed;
                }
            }
        })
        .await
        .expect("terminal event arrives");
        assert!(matches!(
            observed.first(),
            Some(RuntimeEvent::SessionCreated { .. })
        ));
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::RunAccepted { run_id, .. } if run_id == &started.run.run_id
        )));
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::RunStarted { run_id, .. } if run_id == &started.run.run_id
        )));
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolProposed { tool_name, .. } if tool_name == "echo_tool"
        )));
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolOutput {
                channel: assistant_protocol::ToolOutputChannel::Stdout,
                chunk,
                ..
            } if chunk == "hello"
        )));
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::ToolOutput {
                channel: assistant_protocol::ToolOutputChannel::Stderr,
                chunk,
                ..
            } if chunk == "warning"
        )));
        assert_eq!(
            observed
                .iter()
                .filter(|event| matches!(event, RuntimeEvent::RunFinished { .. }))
                .count(),
            1
        );
        assert!(observed.iter().any(|event| matches!(
            event,
            RuntimeEvent::RunFinished {
                run_id,
                status,
                error: None,
                ..
            } if run_id == &terminal.run_id && status == &terminal.status
        )));
        assert_eq!(terminal.tools.len(), 1);
        assert_eq!(terminal.tools[0].stdout, "hello");
        assert_eq!(terminal.tools[0].stderr, "warning");
        assert_eq!(
            terminal.tools[0].status,
            assistant_protocol::ToolActivityStatus::Completed
        );
    }

    #[tokio::test]
    async fn pending_tool_exchange_is_hidden_and_busy_run_cannot_append_user_message() {
        let entered = Arc::new(Notify::new());
        let cleanup = Arc::new(Notify::new());
        let log = OrderLog::new();
        let tool = ScriptedTool::hanging("slow_tool", log)
            .with_entered_signal(entered.clone())
            .with_cleanup_signal(cleanup.clone());
        let mut registry = ToolRegistry::new();
        registry.register(tool).expect("register tool");
        let tools = registry.snapshot();
        let tool_message = assistant_tool_call("assistant-tools", "slow_tool");
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8_192,
            [ModelScript::Events(message_events(&tool_message))],
        ));
        let runtime = runtime(Arc::new(StaticFactory::new(model, tools)));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        let started = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "use the tool".to_owned(),
            })
            .expect("run accepted");

        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("tool entered");
        let during = runtime
            .conversation_snapshot(&session.session.session_id)
            .expect("conversation during tool");
        assert!(matches!(
            during.messages.as_slice(),
            [ConversationMessage::User(_)]
        ));
        assert!(matches!(
            runtime.start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "must not be appended".to_owned(),
            }),
            Err(RuntimeError::SessionBusy { .. })
        ));
        assert_eq!(
            runtime
                .conversation_snapshot(&session.session.session_id)
                .expect("conversation remains unchanged")
                .messages
                .len(),
            1
        );

        runtime
            .cancel_run(CancelRunRequest {
                session_id: session.session.session_id.clone(),
                run_id: started.run.run_id.clone(),
            })
            .expect("cancel active run");
        tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
            .await
            .expect("tool cleanup");
        let terminal =
            wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Cancelled);
        let completed = runtime
            .conversation_snapshot(&session.session.session_id)
            .expect("completed cancellation conversation");
        assert_eq!(completed.messages.len(), 3);
        completed
            .validate_tool_exchange_pairs()
            .expect("cancelled tool exchange remains complete");
    }

    #[tokio::test]
    async fn sessions_run_concurrently_and_cancellation_is_isolated_and_idempotent() {
        let entered = Arc::new(Notify::new());
        let cleanup = Arc::new(Notify::new());
        let runtime = hanging_runtime(2, Some("reused"), entered.clone(), cleanup);
        let mut events = runtime.subscribe_events();
        let first = runtime
            .create_session(CreateSessionRequest::default())
            .expect("first session");
        let second = runtime
            .create_session(CreateSessionRequest::default())
            .expect("second session");

        let first_run = runtime
            .start_run(StartRunRequest {
                session_id: first.session.session_id.clone(),
                message: "first".to_owned(),
            })
            .expect("first run");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("first run entered tool");
        let second_run = runtime
            .start_run(StartRunRequest {
                session_id: second.session.session_id.clone(),
                message: "second".to_owned(),
            })
            .expect("second run while first remains active");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("second run entered tool concurrently");

        let first_cancel = runtime
            .cancel_run(CancelRunRequest {
                session_id: first.session.session_id.clone(),
                run_id: first_run.run.run_id.clone(),
            })
            .expect("first cancellation");
        assert_eq!(
            first_cancel.run.status,
            assistant_protocol::RunStatus::Cancelling
        );
        let repeated = runtime
            .cancel_run(CancelRunRequest {
                session_id: first.session.session_id.clone(),
                run_id: first_run.run.run_id.clone(),
            })
            .expect("repeated cancellation");
        assert_eq!(repeated.run, first_cancel.run);
        let first_terminal =
            wait_for_terminal(&runtime, &first.session.session_id, &first_run.run.run_id).await;
        assert_eq!(
            first_terminal.status,
            assistant_protocol::RunStatus::Cancelled
        );
        assert_eq!(
            runtime
                .cancel_run(CancelRunRequest {
                    session_id: first.session.session_id.clone(),
                    run_id: first_run.run.run_id.clone(),
                })
                .expect("terminal cancellation is idempotent")
                .run,
            first_terminal
        );
        assert_eq!(
            runtime
                .get_run(GetRunRequest {
                    session_id: second.session.session_id.clone(),
                    run_id: second_run.run.run_id.clone(),
                })
                .expect("second run query")
                .run
                .status,
            assistant_protocol::RunStatus::Running
        );

        let reused = runtime
            .start_run(StartRunRequest {
                session_id: first.session.session_id.clone(),
                message: "reuse first session".to_owned(),
            })
            .expect("cancelled session is reusable while another session runs");
        assert_eq!(
            wait_for_terminal(&runtime, &first.session.session_id, &reused.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
        runtime
            .cancel_run(CancelRunRequest {
                session_id: second.session.session_id.clone(),
                run_id: second_run.run.run_id.clone(),
            })
            .expect("second run cleanup cancellation");
        assert_eq!(
            wait_for_terminal(&runtime, &second.session.session_id, &second_run.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Cancelled
        );

        let mut observed = Vec::new();
        while let Ok(event) = events.try_recv() {
            observed.push(event);
        }
        assert_eq!(
            observed
                .iter()
                .filter(|event| matches!(
                    event,
                    RuntimeEvent::RunCancelling { run_id, .. }
                        if run_id == &first_run.run.run_id
                ))
                .count(),
            1
        );
        for run_id in [
            &first_run.run.run_id,
            &second_run.run.run_id,
            &reused.run.run_id,
        ] {
            assert_eq!(
                observed
                    .iter()
                    .filter(|event| matches!(
                        event,
                        RuntimeEvent::RunFinished {
                            run_id: finished,
                            ..
                        } if finished == run_id
                    ))
                    .count(),
                1
            );
        }
        let missing = RunId::new("r_missing").expect("run id");
        assert!(matches!(
            runtime.cancel_run(CancelRunRequest {
                session_id: first.session.session_id,
                run_id: missing.clone(),
            }),
            Err(RuntimeError::RunNotFound { run_id, .. }) if run_id == missing
        ));
    }

    #[tokio::test]
    async fn shutdown_cancels_active_runs_waits_for_settlement_and_is_idempotent() {
        let entered = Arc::new(Notify::new());
        let cleanup = Arc::new(Notify::new());
        let runtime = hanging_runtime(2, None, entered.clone(), cleanup);
        let first = runtime
            .create_session(CreateSessionRequest::default())
            .expect("first session");
        let second = runtime
            .create_session(CreateSessionRequest::default())
            .expect("second session");
        let mut runs = Vec::new();
        for session_id in [&first.session.session_id, &second.session.session_id] {
            let run = runtime
                .start_run(StartRunRequest {
                    session_id: session_id.clone(),
                    message: "hang".to_owned(),
                })
                .expect("run accepted");
            tokio::time::timeout(Duration::from_secs(1), entered.notified())
                .await
                .expect("run entered tool");
            runs.push((session_id.clone(), run.run.run_id));
        }

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            runtime.shutdown(ShutdownRuntimeRequest::default()),
        )
        .await
        .expect("shutdown completes")
        .expect("shutdown succeeds");
        assert_eq!(result.lifecycle, RuntimeLifecycle::Stopped);
        assert_eq!(
            runtime.lifecycle().expect("lifecycle"),
            RuntimeLifecycle::Stopped
        );
        for (session_id, run_id) in &runs {
            let snapshot = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })
                .expect("settled run")
                .run;
            assert_eq!(snapshot.status, assistant_protocol::RunStatus::Cancelled);
            assert!(
                runtime
                    .get_session(GetSessionRequest {
                        session_id: session_id.clone(),
                    })
                    .expect("session")
                    .session
                    .active_run_id
                    .is_none()
            );
        }
        assert!(matches!(
            runtime.create_session(CreateSessionRequest::default()),
            Err(RuntimeError::RuntimeNotRunning {
                lifecycle: RuntimeLifecycle::Stopped
            })
        ));
        assert_eq!(
            runtime
                .shutdown(ShutdownRuntimeRequest::default())
                .await
                .expect("repeated shutdown")
                .lifecycle,
            RuntimeLifecycle::Stopped
        );
    }

    #[tokio::test]
    async fn start_and_shutdown_race_has_no_untracked_active_run() {
        let final_message = assistant_text("assistant-final", "done");
        let model = Arc::new(ScriptedModelService::completing(
            model_capabilities(false),
            8_192,
            final_message,
        ));
        let runtime = Arc::new(runtime(Arc::new(StaticFactory::new(
            model,
            ToolSetSnapshot::default(),
        ))));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        let barrier = Arc::new(Barrier::new(3));
        let start_runtime = runtime.clone();
        let start_barrier = barrier.clone();
        let session_id = session.session.session_id.clone();
        let start = tokio::spawn(async move {
            start_barrier.wait().await;
            start_runtime.start_run(StartRunRequest {
                session_id,
                message: "race".to_owned(),
            })
        });
        let shutdown_runtime = runtime.clone();
        let shutdown_barrier = barrier.clone();
        let shutdown = tokio::spawn(async move {
            shutdown_barrier.wait().await;
            shutdown_runtime
                .shutdown(ShutdownRuntimeRequest::default())
                .await
        });
        barrier.wait().await;

        let start_result = start.await.expect("start task");
        assert_eq!(
            shutdown
                .await
                .expect("shutdown task")
                .expect("shutdown result")
                .lifecycle,
            RuntimeLifecycle::Stopped
        );
        match start_result {
            Ok(started) => {
                assert!(
                    runtime
                        .get_run(GetRunRequest {
                            session_id: session.session.session_id.clone(),
                            run_id: started.run.run_id,
                        })
                        .expect("accepted run was tracked")
                        .run
                        .status
                        .is_terminal()
                );
            }
            Err(RuntimeError::RuntimeNotRunning { .. }) => {}
            Err(error) => panic!("unexpected start result: {error}"),
        }
        assert!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: session.session.session_id,
                })
                .expect("session")
                .session
                .active_run_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_and_compaction_runs_settle_without_automatic_retry() {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [
                ModelScript::FailEstablishment(ModelError::Provider {
                    message: "fixture failure".to_owned(),
                    status: Some(500),
                }),
                ModelScript::FailEstablishment(ModelError::ContextOverflow {
                    message: "fixture overflow".to_owned(),
                }),
            ],
        ));
        let runtime = runtime(Arc::new(StaticFactory::new(
            model.clone(),
            ToolSetSnapshot::default(),
        )));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");

        let failed = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "fail once".to_owned(),
            })
            .expect("failed run accepted");
        let failed =
            wait_for_terminal(&runtime, &session.session.session_id, &failed.run.run_id).await;
        assert_eq!(failed.status, assistant_protocol::RunStatus::Failed);
        assert!(failed.error.is_some());

        let compact = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "overflow once".to_owned(),
            })
            .expect("compaction run accepted");
        let compact =
            wait_for_terminal(&runtime, &session.session.session_id, &compact.run.run_id).await;
        assert_eq!(
            compact.status,
            assistant_protocol::RunStatus::CompactionRequired
        );
        assert_eq!(model.take_requests().len(), 2);
        assert_eq!(
            runtime
                .conversation_snapshot(&session.session.session_id)
                .expect("conversation")
                .messages
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn completion_panic_is_caught_and_session_is_not_left_busy() {
        let model = Arc::new(PanicModel {
            capabilities: model_capabilities(false),
        });
        let runtime = runtime(Arc::new(StaticFactory::new(
            model,
            ToolSetSnapshot::default(),
        )));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        let started = runtime
            .start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: "panic".to_owned(),
            })
            .expect("run accepted");

        let terminal =
            wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Failed);
        assert_eq!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: session.session.session_id,
                })
                .expect("session query")
                .session
                .active_run_id,
            None
        );
    }

    #[tokio::test]
    async fn blank_message_and_unknown_run_do_not_mutate_conversation() {
        let runtime = runtime(Arc::new(CountingFactory::new()));
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .expect("session");
        assert!(matches!(
            runtime.start_run(StartRunRequest {
                session_id: session.session.session_id.clone(),
                message: " \n\t".to_owned(),
            }),
            Err(RuntimeError::InvalidRequest { .. })
        ));
        assert!(
            runtime
                .conversation_snapshot(&session.session.session_id)
                .expect("conversation")
                .messages
                .is_empty()
        );
        let missing = RunId::new("r_missing").expect("run id");
        assert!(matches!(
            runtime.get_run(GetRunRequest {
                session_id: session.session.session_id.clone(),
                run_id: missing.clone(),
            }),
            Err(RuntimeError::RunNotFound { run_id, .. }) if run_id == missing
        ));
    }
}
