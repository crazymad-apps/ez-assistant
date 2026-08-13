//! AssistantRuntime 生命周期门禁和 Session Registry。

mod approval;
mod attachment;
mod connection_validation;
mod delegation;
mod input;
mod model;
mod permission;
mod recovery;
mod session_management;
mod shutdown;
mod tasks;
mod workspace;

pub use attachment::StagedAttachmentUpload;

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use agent_sdk::ContextWindowEvaluator;
use agent_types::ConversationSnapshot;
use assistant_protocol::{
    AgentVariant, ApprovalMode, AttachmentId, CancelRunRequest, CancelRunResult,
    ConfigurationStatus, CreateSessionRequest, CreateSessionResult, GetConfigStatusRequest,
    GetConfigStatusResult, GetModelRequest, GetModelResult, GetRunRequest, GetRunResult,
    GetSessionRequest, GetSessionResult, ListModelsRequest, ListModelsResult, ListSessionsRequest,
    ListSessionsResult, ReloadConfigRequest, ReloadConfigResult, RuntimeEvent, RuntimeLifecycle,
    SessionId, SessionSummary, WorkspaceId,
};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock, broadcast};
use tokio_util::sync::CancellationToken;

use self::model::resolve_session_model_key;
use self::recovery::recover_registries;
use self::tasks::RuntimeTasks;
use crate::{
    ChildTaskWorkspaceFactory, ModelServiceFactory, NewStoredSession, RecoveredRuntime,
    RunToolFactory, RuntimeConfig, RuntimeConfigSource, RuntimeError, RuntimeResult, RuntimeStore,
    SessionEnvironmentFactory, SessionEnvironmentFactoryRequest, StoreErrorKind, StoredWorkspace,
    WorkspaceEnvironmentSource,
    config::{
        ConfigRegistry, ConfigSnapshot, project_model_by_key, project_models, project_status,
    },
    delegation::ChildTaskRegistry,
    permission::{PermissionCoordinator, PermissionFileScope, VolatilePermissionFileStore},
    session::{SessionController, allocate_session_id},
};

const CONTEXT_WINDOW_THRESHOLD: f64 = 0.8;

pub(crate) fn now_ms() -> RuntimeResult<i64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "system clock",
        }
    })?;
    i64::try_from(duration.as_millis()).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "system clock range",
    })
}

/// 应用业务 Runtime 的进程内权威入口。
///
/// Drop 只释放内存，不冒充异步关闭；宿主退出前必须显式调用 [`Self::shutdown`]。
pub struct AssistantRuntime {
    config: RuntimeConfig,
    lifecycle: RwLock<RuntimeLifecycle>,
    sessions: RwLock<BTreeMap<SessionId, Arc<SessionController>>>,
    workspaces: RwLock<BTreeMap<WorkspaceId, StoredWorkspace>>,
    attachments: RwLock<BTreeMap<AttachmentId, crate::StoredAttachment>>,
    store: Arc<dyn RuntimeStore>,
    operation_gate: AsyncRwLock<()>,
    workspace_mutation_gate: AsyncMutex<()>,
    config_registry: Arc<ConfigRegistry>,
    permission_coordinator: Arc<PermissionCoordinator>,
    approval_registry: Arc<crate::permission::ApprovalRegistry>,
    model_factory: Arc<dyn ModelServiceFactory>,
    session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
    run_tool_factory: Arc<dyn RunToolFactory>,
    child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    child_tasks: Arc<ChildTaskRegistry>,
    context_window: Arc<ContextWindowEvaluator>,
    event_sender: broadcast::Sender<RuntimeEvent>,
    root_cancellation: CancellationToken,
    tasks: RuntimeTasks,
}

impl AssistantRuntime {
    /// 创建不跨进程保留数据的易失 Runtime，供无 Host 的嵌入式调用与单元测试使用。
    ///
    /// 正式 Runtime Host 必须使用 [`Self::open`] 注入产品 Store 并先完成启动恢复。
    pub fn new(
        config: RuntimeConfig,
        config_source: Arc<dyn RuntimeConfigSource>,
        model_factory: Arc<dyn ModelServiceFactory>,
        session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
        run_tool_factory: Arc<dyn RunToolFactory>,
        child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    ) -> Self {
        let permission_store = Arc::new(VolatilePermissionFileStore::default());
        let permission_coordinator = Arc::new(PermissionCoordinator::empty(permission_store));
        permission_coordinator
            .register_empty_scope(PermissionFileScope::Global)
            .expect("empty global permission scope is valid");
        Self::from_recovered(
            config,
            config_source,
            model_factory,
            session_environment_factory,
            run_tool_factory,
            child_task_workspace_factory,
            Arc::new(crate::storage::VolatileRuntimeStore::default()),
            permission_coordinator,
            RecoveredRuntime::default(),
        )
        .expect("empty volatile runtime state is valid")
    }

    /// 恢复 Store 后创建 Running Runtime；返回前不会开放任何客户端业务入口。
    #[allow(clippy::too_many_arguments)]
    pub async fn open(
        config: RuntimeConfig,
        config_source: Arc<dyn RuntimeConfigSource>,
        model_factory: Arc<dyn ModelServiceFactory>,
        session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
        run_tool_factory: Arc<dyn RunToolFactory>,
        child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
        store: Arc<dyn RuntimeStore>,
        permission_store: Arc<dyn crate::PermissionFileStore>,
    ) -> RuntimeResult<Self> {
        let recovered = store
            .load_runtime()
            .await
            .map_err(|source| RuntimeError::from_store("recover runtime", source))?;
        let permission_scopes = permission_scopes(&recovered);
        let permission_coordinator =
            Arc::new(PermissionCoordinator::open(permission_store, permission_scopes).await);
        Self::from_recovered(
            config,
            config_source,
            model_factory,
            session_environment_factory,
            run_tool_factory,
            child_task_workspace_factory,
            store,
            permission_coordinator,
            recovered,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_recovered(
        config: RuntimeConfig,
        config_source: Arc<dyn RuntimeConfigSource>,
        model_factory: Arc<dyn ModelServiceFactory>,
        session_environment_factory: Arc<dyn SessionEnvironmentFactory>,
        run_tool_factory: Arc<dyn RunToolFactory>,
        child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
        store: Arc<dyn RuntimeStore>,
        permission_coordinator: Arc<PermissionCoordinator>,
        recovered: RecoveredRuntime,
    ) -> RuntimeResult<Self> {
        let (event_sender, _) = broadcast::channel(config.event_capacity.get());
        let recovered = recover_registries(recovered)?;
        Ok(Self {
            config,
            lifecycle: RwLock::new(RuntimeLifecycle::Running),
            sessions: RwLock::new(recovered.sessions),
            workspaces: RwLock::new(recovered.workspaces),
            attachments: RwLock::new(recovered.attachments),
            store,
            operation_gate: AsyncRwLock::new(()),
            workspace_mutation_gate: AsyncMutex::new(()),
            config_registry: Arc::new(ConfigRegistry::new(config_source)),
            permission_coordinator,
            approval_registry: Arc::new(crate::permission::ApprovalRegistry::new()),
            model_factory,
            session_environment_factory,
            run_tool_factory,
            child_task_workspace_factory,
            child_tasks: Arc::new(ChildTaskRegistry::recovered(recovered.child_tasks)),
            context_window: Arc::new(
                ContextWindowEvaluator::new(CONTEXT_WINDOW_THRESHOLD)
                    .expect("static context window threshold is valid"),
            ),
            event_sender,
            root_cancellation: CancellationToken::new(),
            tasks: RuntimeTasks::new(),
        })
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

    /// 创建一个带初始 model key、冻结 System Prompt 和空 Conversation 的 Session。
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> RuntimeResult<CreateSessionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _workspace_mutation = self.workspace_mutation_gate.lock().await;

        let title = request.title.unwrap_or_else(|| "New Session".to_owned());
        let config_snapshot = self.config_registry.snapshot()?;
        let model_key = resolve_session_model_key(&config_snapshot, request.model_key)?;

        let session_id = {
            let sessions =
                self.sessions
                    .read()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "session registry",
                    })?;
            allocate_session_id(&sessions)?
        };
        let workspace = request
            .workspace_id
            .as_ref()
            .map(|workspace_id| self.workspace_for_new_session(workspace_id))
            .transpose()?;
        let prepared = self
            .session_environment_factory
            .create_environment(SessionEnvironmentFactoryRequest {
                session_id: &session_id,
                workspace: workspace
                    .as_ref()
                    .map(|workspace| WorkspaceEnvironmentSource {
                        workspace_id: &workspace.workspace_id,
                        user_directory: &workspace.user_directory,
                        agent_directory: &workspace.agent_directory,
                    }),
            })
            .map_err(|source| RuntimeError::SessionEnvironmentBuildFailed { source })?;
        let stored = self
            .store
            .create_session(NewStoredSession {
                session_id: session_id.clone(),
                title,
                model_key,
                system_prompt: prepared.system_prompt,
                environment: prepared.environment,
                current_variant: AgentVariant::Build,
                approval_mode: ApprovalMode::Ask,
                created_at_ms: now_ms()?,
            })
            .await
            .map_err(|source| {
                if source.kind() == StoreErrorKind::ResourceUnavailable
                    && let Some(workspace_id) = request.workspace_id
                {
                    return RuntimeError::WorkspaceUnavailable { workspace_id };
                }
                RuntimeError::from_store("create session", source)
            })?;
        self.permission_coordinator
            .register_empty_scope(PermissionFileScope::Session(session_id.clone()))?;
        let session = Arc::new(SessionController::new(stored));
        let mut sessions =
            self.sessions
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "session registry",
                })?;
        match sessions.entry(session_id.clone()) {
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

        let summary = session.summary()?;
        self.publish(RuntimeEvent::SessionCreated {
            session: summary.clone(),
        });
        Ok(CreateSessionResult { session: summary })
    }

    /// 按 SessionId 的确定性顺序列出当前进程内 Session。
    pub fn list_sessions(&self, request: ListSessionsRequest) -> RuntimeResult<ListSessionsResult> {
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
            .collect::<RuntimeResult<Vec<SessionSummary>>>()?
            .into_iter()
            .filter(|session| match request.filter {
                assistant_protocol::SessionListFilter::Active => {
                    session.lifecycle == assistant_protocol::SessionLifecycle::Active
                }
                assistant_protocol::SessionListFilter::Archived => {
                    session.lifecycle == assistant_protocol::SessionLifecycle::Archived
                }
                assistant_protocol::SessionListFilter::All => true,
            })
            .collect();
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
    pub async fn conversation_snapshot(
        &self,
        session_id: &SessionId,
    ) -> RuntimeResult<ConversationSnapshot> {
        let session = self.session(session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        session.conversation_snapshot()
    }

    /// 查询指定 Session 中的 Runtime Run 快照。
    pub async fn get_run(&self, request: GetRunRequest) -> RuntimeResult<GetRunResult> {
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        Ok(GetRunResult {
            run: session.run_snapshot(&request.run_id)?,
        })
    }

    /// 请求取消一个活动 Run；终态 Run 重复取消会原样返回当前快照。
    pub async fn cancel_run(&self, request: CancelRunRequest) -> RuntimeResult<CancelRunResult> {
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        self.cancel_run_approvals(&request.session_id, &request.run_id)
            .await?;
        let (snapshot, cancellation) = {
            let mut state = session.lock_state()?;
            let existing =
                state
                    .runs
                    .get(&request.run_id)
                    .ok_or_else(|| RuntimeError::RunNotFound {
                        session_id: request.session_id.clone(),
                        run_id: request.run_id.clone(),
                    })?;
            if existing.snapshot().status.is_terminal() {
                return Ok(CancelRunResult {
                    run: existing.snapshot(),
                });
            }
            if state
                .active_run
                .as_ref()
                .is_none_or(|active| active.run_id != request.run_id)
            {
                return Err(RuntimeError::InvalidRequest {
                    reason: "queued input must be cancelled with cancel_queued_input",
                });
            }
            let record = state.runs.get_mut(&request.run_id).ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "active run record",
                },
            )?;
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

    fn ensure_running(&self) -> RuntimeResult<()> {
        let lifecycle = self.lifecycle()?;
        if lifecycle == RuntimeLifecycle::Running {
            Ok(())
        } else {
            Err(RuntimeError::RuntimeNotRunning { lifecycle })
        }
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

fn permission_scopes(recovered: &RecoveredRuntime) -> Vec<PermissionFileScope> {
    let mut scopes = vec![PermissionFileScope::Global];
    scopes.extend(
        recovered
            .workspaces
            .iter()
            .map(|workspace| PermissionFileScope::Workspace(workspace.workspace_id.clone())),
    );
    scopes.extend(
        recovered
            .sessions
            .iter()
            .map(|session| PermissionFileScope::Session(session.session_id.clone())),
    );
    scopes.sort();
    scopes.dedup();
    scopes
}

#[cfg(test)]
mod tests;
