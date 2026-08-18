//! Run 与连接验证共用的模型服务编译。

use std::{sync::Arc, time::Duration};

use agent_core::{ExecutionBudget, ToolAuthorizer};
use agent_model::{
    ModelAttemptEvent, ModelAttemptObserver, ModelService, ModelStreamFuture, ProviderOptions,
    ReasoningConfig, RetryingModelService, SystemPromptSnapshot,
};
use agent_sdk::{Agent, AgentBuilder};
use agent_types::ToolChoice;
use assistant_protocol::{AgentVariant, ApprovalMode, ModelKey};

use super::AssistantRuntime;
use crate::{
    ChildTaskWorkspaceFactory, ModelCompatibilityProfile, ModelServiceFactoryRequest,
    RunToolFactory, RunToolFactoryErrorKind, RuntimeError, RuntimeResult, RuntimeStore,
    config::{ConfigSnapshot, ResolvedModelConfig},
    context_compaction::RuntimeContextCompactor,
    delegation::{
        ChildTaskRegistry, DelegateTaskTool, ParentDelegationController, ParentDelegationResources,
    },
    observation::ObservationCoordinator,
    permission::{
        ApprovalRegistry, PermissionCoordinator, RunAuthorizationScope, RuntimeApprovalResolver,
        RuntimeToolAuthorizer,
    },
    session::SessionController,
};

/// 一次配置快照编译出的模型调用边界。
///
/// Run 和连接验证共用这条构造链，避免两者对 endpoint、credential、Profile、
/// timeout 和 retry 产生不同解释；两者的请求内容仍分别构造。
pub(super) struct CompiledModelService {
    pub(super) model: Arc<dyn ModelService>,
    pub(super) profile: ModelCompatibilityProfile,
    pub(super) max_output_tokens: u32,
    pub(super) request_timeout: Duration,
}

/// 未启用重试时只补充 attempt 观察，不改变下层取消、超时或建流语义。
struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    observer: Arc<dyn ModelAttemptObserver>,
}

impl ModelService for ObservedModelService {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(
        &self,
        request: agent_model::ModelRequest,
        context: agent_model::ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            let trace = context.trace.clone();
            self.observer.observe(ModelAttemptEvent::Started {
                trace: trace.clone(),
                attempt: 1,
            });
            match self.inner.stream(request, context).await {
                Ok(stream) => {
                    self.observer
                        .observe(ModelAttemptEvent::StreamEstablished { trace, attempt: 1 });
                    Ok(stream)
                }
                Err(error) => {
                    self.observer
                        .observe(ModelAttemptEvent::EstablishmentFailed {
                            trace,
                            attempt: 1,
                            error: error.clone(),
                            retry_reason: None,
                            will_retry: false,
                        });
                    Err(error)
                }
            }
        })
    }
}

/// 单次 Run 已同时冻结 Agent 规格和对应授权闸。
pub(super) struct CompiledRunAgent {
    agent: Agent,
    authorizer: Arc<dyn ToolAuthorizer>,
    compactor: Arc<RuntimeContextCompactor>,
}

impl CompiledRunAgent {
    pub(super) fn into_parts(
        self,
    ) -> (Agent, Arc<dyn ToolAuthorizer>, Arc<RuntimeContextCompactor>) {
        (self.agent, self.authorizer, self.compactor)
    }
}

pub(super) struct RunAuthorizationInput {
    pub(super) permission_coordinator: Arc<PermissionCoordinator>,
    pub(super) approval_registry: Arc<ApprovalRegistry>,
    pub(super) variant: AgentVariant,
    pub(super) approval_mode: ApprovalMode,
    pub(super) run_id: assistant_protocol::RunId,
    pub(super) cancellation: tokio_util::sync::CancellationToken,
    pub(super) events: ObservationCoordinator,
}

/// 队列驱动与历史重入共同传入的 Run 装配资源；收敛参数数量并明确哪些能力来自 Runtime。
pub(super) struct RunCompilationResources<'a> {
    pub(super) model_factory: &'a dyn crate::ModelServiceFactory,
    pub(super) context_window: Arc<agent_sdk::ContextWindowEvaluator>,
    pub(super) run_tool_factory: &'a dyn RunToolFactory,
    pub(super) child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    pub(super) child_tasks: Arc<ChildTaskRegistry>,
    pub(super) store: Arc<dyn RuntimeStore>,
    pub(super) recall_reference_codec: Arc<crate::HmacRecallReferenceCodec>,
}

impl AssistantRuntime {
    /// 从同一配置快照构造 Run 和连接验证共用的冻结 ModelService。
    pub(super) fn compile_model_service(
        &self,
        snapshot: &ConfigSnapshot,
        model_key: &assistant_protocol::ModelKey,
    ) -> RuntimeResult<CompiledModelService> {
        compile_model_service(snapshot, model_key, self.model_factory.as_ref())
    }
}

pub(super) fn compile_run_agent(
    session: Arc<SessionController>,
    snapshot: &ConfigSnapshot,
    resources: RunCompilationResources<'_>,
    authorization: RunAuthorizationInput,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledRunAgent> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let model_key = session.model_key()?;
    let model_config = resolve_model(snapshot, &model_key)?;
    let compiled = compile_model_service_with_observer(
        snapshot,
        &model_key,
        resources.model_factory,
        model_attempt_observer,
    )?;
    let (reasoning, provider_options) = profile_request_options(compiled.profile)?;
    // Conversation Recall 同时具备检索和稳定引用续读能力，但两个 trait 保持独立，避免将
    // 有序续读语义强加给所有通用 Recall Source。
    let conversation_recall = Arc::new(crate::conversation_recall::RuntimeConversationRecall::new(
        resources.store.clone(),
        resources.recall_reference_codec.clone(),
        session.id().clone(),
        session.environment().workspace_id.clone(),
    ));
    let bundle = resources
        .run_tool_factory
        .compile(crate::RunToolFactoryRequest {
            session_id: session.id(),
            environment: session.environment(),
            pinned_memory: Arc::new(crate::RuntimePinnedMemoryStore::new(
                resources.store.clone(),
                session.id().clone(),
            )),
            conversation_recall: conversation_recall.clone(),
            conversation_recall_reader: conversation_recall,
        })
        .map_err(|source| {
            if source.kind() == RunToolFactoryErrorKind::WorkingDirectoryUnavailable
                && let Some(workspace_id) = session.environment().workspace_id.clone()
            {
                return RuntimeError::WorkspaceUnavailable { workspace_id };
            }
            RuntimeError::RunToolsBuildFailed { source }
        })?;
    let (base_tools, infrastructure_policies) = bundle.into_parts();
    let parent_compactor = Arc::new(RuntimeContextCompactor::for_parent(
        compiled.model.clone(),
        session.system_prompt().clone(),
    ));
    let session_id = session.id().clone();
    let run_id = authorization.run_id.clone();
    let authorizer = Arc::new(RuntimeToolAuthorizer::new(
        RunAuthorizationScope {
            variant: authorization.variant,
            approval_mode: authorization.approval_mode,
        },
        session.permission_scopes(),
        authorization.permission_coordinator.clone(),
        infrastructure_policies.clone(),
        session.environment(),
        Arc::new(RuntimeApprovalResolver {
            registry: authorization.approval_registry.clone(),
            session_id,
            run_id,
            child_task_id: None,
            variant: authorization.variant,
            approval_mode: authorization.approval_mode,
            workspace_id: session.environment().workspace_id.clone(),
            cancellation: authorization.cancellation.clone(),
            events: authorization.events.clone(),
        }),
    )?);

    let model_request = agent_core::ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: model_config.generation().clone(),
        reasoning,
        provider_options,
    };
    // 不具备 Tool Call 能力的模型维持历史纯文本路径；一旦模型支持工具，父 Agent
    // 才派生 delegate_task，而子 Agent 始终只拿到原始 Base ToolSet。
    let parent_tools = if compiled.model.capabilities().tool_calls {
        let delegation = active.delegation();
        let mut child_generation = model_request.generation.clone();
        child_generation.max_output_tokens = Some(
            child_generation
                .max_output_tokens
                .unwrap_or(u32::MAX)
                .min(delegation.max_output_tokens().get()),
        );
        let child_budget = ExecutionBudget {
            max_steps: Some(
                active
                    .budget()
                    .max_steps
                    .unwrap_or(u32::MAX)
                    .min(delegation.max_steps().get()),
            ),
            max_tool_calls: Some(
                active
                    .budget()
                    .max_tool_calls
                    .unwrap_or(u32::MAX)
                    .min(delegation.max_tool_calls().get()),
            ),
        };
        let mut child_prompt_parts = session.system_prompt().parts().to_vec();
        child_prompt_parts.push(crate::delegation::CHILD_AGENT_INSTRUCTION_V1.to_owned());
        let child_prompt = SystemPromptSnapshot::new(child_prompt_parts);
        let child_compactor = Arc::new(RuntimeContextCompactor::for_child(
            compiled.model.clone(),
            child_prompt.clone(),
        ));
        let mut child_builder = AgentBuilder::new(
            compiled.model.clone(),
            child_prompt,
            resources.context_window.clone(),
        )
        .tools(base_tools.clone())
        .model_request(agent_core::ModelRequestConfig {
            tool_choice: ToolChoice::Auto,
            generation: child_generation,
            reasoning: model_request.reasoning.clone(),
            provider_options: model_request.provider_options.clone(),
        })
        .budget(child_budget);
        if active.guardrails().repeated_invocation.is_some()
            || active.guardrails().consecutive_failures.is_some()
        {
            child_builder = child_builder.guardrails(active.guardrails().clone());
        }
        let child_agent = Arc::new(
            child_builder
                .build()
                .map_err(|source| RuntimeError::AgentBuildFailed { source })?,
        );
        let delegation_controller =
            Arc::new(ParentDelegationController::new(ParentDelegationResources {
                session: session.clone(),
                parent_run_id: authorization.run_id,
                variant: authorization.variant,
                approval_mode: authorization.approval_mode,
                child_agent,
                child_compactor,
                store: resources.store,
                registry: resources.child_tasks,
                workspace_factory: resources.child_task_workspace_factory,
                permission_coordinator: authorization.permission_coordinator,
                approval_registry: authorization.approval_registry,
                infrastructure_policies,
                events: authorization.events,
                limits: delegation,
            }));
        base_tools
            .try_with_tool(DelegateTaskTool::new(delegation_controller))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "delegate task tool definition",
            })?
    } else {
        base_tools
    };

    let mut builder = AgentBuilder::new(
        compiled.model,
        session.system_prompt().clone(),
        resources.context_window,
    )
    .tools(parent_tools)
    .model_request(model_request)
    .budget(active.budget().clone());
    if active.guardrails().repeated_invocation.is_some()
        || active.guardrails().consecutive_failures.is_some()
    {
        builder = builder.guardrails(active.guardrails().clone());
    }
    let agent = builder
        .build()
        .map_err(|source| RuntimeError::AgentBuildFailed { source })?;
    Ok(CompiledRunAgent {
        agent,
        authorizer,
        compactor: parent_compactor,
    })
}

pub(super) fn compile_model_service(
    snapshot: &ConfigSnapshot,
    model_key: &assistant_protocol::ModelKey,
    model_factory: &dyn crate::ModelServiceFactory,
) -> RuntimeResult<CompiledModelService> {
    compile_model_service_with_observer(snapshot, model_key, model_factory, None)
}

fn compile_model_service_with_observer(
    snapshot: &ConfigSnapshot,
    model_key: &assistant_protocol::ModelKey,
    model_factory: &dyn crate::ModelServiceFactory,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledModelService> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let model_config = resolve_model(snapshot, model_key)?;
    let transport = active.transport();
    let profile = model_config.compatibility_profile();
    let base_model = model_factory
        .create_model(ModelServiceFactoryRequest {
            provider: model_config.provider(),
            profile,
            endpoint: model_config.endpoint(),
            model: model_config.model(),
            api_key: model_config.api_key(),
            context_window_tokens: model_config.context_window_tokens(),
            connect_timeout: transport.connect_timeout(),
            request_timeout: transport.request_timeout(),
        })
        .map_err(|source| RuntimeError::ModelBuildFailed { source })?;
    let model = match (active.retry_policy(), model_attempt_observer) {
        (Some(policy), Some(observer)) => Arc::new(RetryingModelService::with_observer(
            base_model,
            policy.clone(),
            observer,
        )) as Arc<dyn ModelService>,
        (Some(policy), None) => {
            Arc::new(RetryingModelService::new(base_model, policy.clone())) as Arc<dyn ModelService>
        }
        (None, Some(observer)) => Arc::new(ObservedModelService {
            inner: base_model,
            observer,
        }) as Arc<dyn ModelService>,
        (None, None) => base_model,
    };
    Ok(CompiledModelService {
        model,
        profile,
        max_output_tokens: model_config.max_output_tokens(),
        request_timeout: transport.request_timeout(),
    })
}

/// 按 Profile 编译业务请求必需的 reasoning 和 Provider Options。
pub(super) fn profile_request_options(
    profile: ModelCompatibilityProfile,
) -> RuntimeResult<(Option<ReasoningConfig>, ProviderOptions)> {
    let mut provider_options = ProviderOptions::new();
    let reasoning = if profile == ModelCompatibilityProfile::DeepSeek {
        provider_options
            .insert(
                "deepseek",
                serde_json::json!({"thinking": {"type": "enabled"}}),
            )
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "static DeepSeek provider options",
            })?;
        Some(ReasoningConfig { effort: None })
    } else {
        None
    };
    Ok((reasoning, provider_options))
}

/// CreateSession 在同一配置快照中解析显式或默认 model key。
pub(super) fn resolve_session_model_key(
    snapshot: &ConfigSnapshot,
    requested: Option<ModelKey>,
) -> RuntimeResult<ModelKey> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let key = requested
        .or_else(|| active.default_model().cloned())
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    resolve_model(snapshot, &key)?;
    Ok(key)
}

/// 在安全投影和有效 map 之间区分不存在与存在但无效。
fn resolve_model<'a>(
    snapshot: &'a ConfigSnapshot,
    key: &ModelKey,
) -> RuntimeResult<&'a ResolvedModelConfig> {
    if let Some(model) = snapshot.model(key) {
        return Ok(model);
    }
    if snapshot.contains_model_key(key) {
        Err(RuntimeError::ModelUnavailable {
            model_key: key.clone(),
        })
    } else {
        Err(RuntimeError::ModelNotFound {
            model_key: key.clone(),
        })
    }
}
