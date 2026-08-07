//! AssistantRuntime 生命周期门禁和 Session Registry。

mod connection_validation;
mod model;
mod tasks;

use std::{
    collections::{BTreeMap, btree_map::Entry},
    panic::AssertUnwindSafe,
    sync::{Arc, RwLock},
};

use agent_core::{AgentExecution, ExecutionContext, ExecutionInput, ToolAuthorizer};
use agent_sdk::ContextWindowEvaluator;
use agent_tools::ToolSetSnapshot;
use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{
    CancelRunRequest, CancelRunResult, ConfigurationStatus, CreateSessionRequest,
    CreateSessionResult, GetConfigStatusRequest, GetConfigStatusResult, GetModelRequest,
    GetModelResult, GetRunRequest, GetRunResult, GetSessionRequest, GetSessionResult,
    ListModelsRequest, ListModelsResult, ListSessionsRequest, ListSessionsResult,
    ReloadConfigRequest, ReloadConfigResult, RuntimeEvent, RuntimeLifecycle, SessionId,
    SessionSummary, ShutdownRuntimeRequest, ShutdownRuntimeResult, StartRunRequest, StartRunResult,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use self::model::resolve_session_model_key;
use self::tasks::RuntimeTasks;
use crate::{
    ModelServiceFactory, RuntimeConfig, RuntimeConfigSource, RuntimeError, RuntimeResult,
    SystemPromptFactory,
    config::{
        ConfigRegistry, ConfigSnapshot, project_model_by_key, project_models, project_status,
    },
    run::{
        ActiveRun, RunRecord, RuntimeRecorder, allocate_run_id, create_user_message,
        finished_event, settle_run, supervise_run,
    },
    session::{SessionController, allocate_session_id},
};

const CONTEXT_WINDOW_THRESHOLD: f64 = 0.8;

/// 应用业务 Runtime 的进程内权威入口。
///
/// Drop 只释放内存，不冒充异步关闭；宿主退出前必须显式调用 [`Self::shutdown`]。
pub struct AssistantRuntime {
    config: RuntimeConfig,
    lifecycle: RwLock<RuntimeLifecycle>,
    sessions: RwLock<BTreeMap<SessionId, Arc<SessionController>>>,
    config_registry: ConfigRegistry,
    model_factory: Arc<dyn ModelServiceFactory>,
    system_prompt_factory: Arc<dyn SystemPromptFactory>,
    tools: ToolSetSnapshot,
    context_window: Arc<ContextWindowEvaluator>,
    default_authorizer: Arc<dyn ToolAuthorizer>,
    event_sender: broadcast::Sender<RuntimeEvent>,
    root_cancellation: CancellationToken,
    tasks: RuntimeTasks,
}

impl AssistantRuntime {
    /// 使用显式 bootstrap 配置、配置源、装配工厂和默认授权闸创建 Running Runtime。
    pub fn new(
        config: RuntimeConfig,
        config_source: Arc<dyn RuntimeConfigSource>,
        model_factory: Arc<dyn ModelServiceFactory>,
        system_prompt_factory: Arc<dyn SystemPromptFactory>,
        tools: ToolSetSnapshot,
        default_authorizer: Arc<dyn ToolAuthorizer>,
    ) -> Self {
        let (event_sender, _) = broadcast::channel(config.event_capacity.get());
        Self {
            config,
            lifecycle: RwLock::new(RuntimeLifecycle::Running),
            sessions: RwLock::new(BTreeMap::new()),
            config_registry: ConfigRegistry::new(config_source),
            model_factory,
            system_prompt_factory,
            tools,
            context_window: Arc::new(
                ContextWindowEvaluator::new(CONTEXT_WINDOW_THRESHOLD)
                    .expect("static context window threshold is valid"),
            ),
            default_authorizer,
            event_sender,
            root_cancellation: CancellationToken::new(),
            tasks: RuntimeTasks::new(),
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

    /// 查询当前配置总体状态；结果只来自脱敏投影。
    pub fn get_config_status(
        &self,
        _request: GetConfigStatusRequest,
    ) -> RuntimeResult<GetConfigStatusResult> {
        let snapshot = self.config_registry.snapshot()?;
        Ok(GetConfigStatusResult {
            status: self.configuration_status(&snapshot),
        })
    }

    /// 按配置中的确定性顺序列出全部模型脱敏投影。
    pub fn list_models(&self, _request: ListModelsRequest) -> RuntimeResult<ListModelsResult> {
        let snapshot = self.config_registry.snapshot()?;
        Ok(ListModelsResult {
            models: project_models(snapshot.projection()),
        })
    }

    /// 查询指定 model key 的脱敏投影，包括无效模型的安全诊断。
    pub fn get_model(&self, request: GetModelRequest) -> RuntimeResult<GetModelResult> {
        let snapshot = self.config_registry.snapshot()?;
        let model =
            project_model_by_key(snapshot.projection(), &request.model_key).ok_or_else(|| {
                RuntimeError::ModelNotFound {
                    model_key: request.model_key,
                }
            })?;
        Ok(GetModelResult { model })
    }

    /// 从唯一配置源重新加载并原子替换快照；配置错误作为正常诊断结果返回。
    pub async fn reload_config(
        &self,
        _request: ReloadConfigRequest,
    ) -> RuntimeResult<ReloadConfigResult> {
        self.ensure_running()?;
        let snapshot = self.config_registry.reload().await?;
        Ok(ReloadConfigResult {
            status: self.configuration_status(&snapshot),
        })
    }

    /// 创建一个冻结 model key、System Prompt 和空 Conversation 的 Session。
    pub fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<CreateSessionResult> {
        self.ensure_running()?;

        let title = request.title.unwrap_or_else(|| "New Session".to_owned());
        let config_snapshot = self.config_registry.snapshot()?;
        let model_key = resolve_session_model_key(&config_snapshot, request.model_key)?;
        let system_prompt = self
            .system_prompt_factory
            .create_system_prompt()
            .map_err(|source| RuntimeError::SystemPromptBuildFailed { source })?;

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
        let session = Arc::new(SessionController::new(
            session_id.clone(),
            title,
            model_key,
            system_prompt,
        ));
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
        {
            let state = session.lock_state()?;
            state.ensure_can_start(session.id())?;
        }
        let config_snapshot = self.config_registry.snapshot()?;
        let agent = self.compile_run_agent(&session, &config_snapshot)?;
        let user_message = create_user_message(request.message)?;
        let cancellation = self.root_cancellation.child_token();
        let (run_id, accepted, input) = {
            let mut state = session.lock_state()?;
            state.ensure_can_start(session.id())?;
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
            agent.start(
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
                    self.publish(finished_event(snapshot));
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
        let graceful = self.tasks.wait_or_abort(self.config.shutdown_timeout).await;
        if !graceful {
            self.force_settle_active_runs()?;
        }

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

    /// supervisor 超时被中止后，不将 Cancelling Run 遗留为非终态权威事实。
    fn force_settle_active_runs(&self) -> RuntimeResult<()> {
        let sessions: Vec<_> = self
            .sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .values()
            .cloned()
            .collect();
        for session in sessions {
            let run_id = session
                .lock_state()?
                .active_run
                .as_ref()
                .map(|active| active.run_id.clone());
            if let Some(run_id) = run_id {
                let snapshot = settle_run(&session, &run_id, None)?;
                self.publish(finished_event(snapshot));
            }
        }
        Ok(())
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

    fn configuration_status(&self, snapshot: &ConfigSnapshot) -> ConfigurationStatus {
        project_status(snapshot.projection(), self.config_registry.display_path())
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

#[cfg(test)]
mod tests;
