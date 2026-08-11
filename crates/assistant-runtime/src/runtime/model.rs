//! Run 与连接验证共用的模型服务编译。

use std::{sync::Arc, time::Duration};

use agent_core::ToolAuthorizer;
use agent_model::{
    ModelAttemptEvent, ModelAttemptObserver, ModelService, ModelStreamFuture, ProviderOptions,
    ReasoningConfig, RetryingModelService,
};
use agent_sdk::{Agent, AgentBuilder};
use agent_types::ToolChoice;
use assistant_protocol::ModelKey;

use super::AssistantRuntime;
use crate::{
    ModelCompatibilityProfile, ModelServiceFactoryRequest, RunToolFactory, RunToolFactoryErrorKind,
    RuntimeError, RuntimeResult,
    config::{ConfigSnapshot, ResolvedModelConfig},
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
}

impl CompiledRunAgent {
    pub(super) fn into_parts(self) -> (Agent, Arc<dyn ToolAuthorizer>) {
        (self.agent, self.authorizer)
    }
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
    session: &SessionController,
    snapshot: &ConfigSnapshot,
    model_factory: &dyn crate::ModelServiceFactory,
    context_window: Arc<agent_sdk::ContextWindowEvaluator>,
    run_tool_factory: &dyn RunToolFactory,
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
        model_factory,
        model_attempt_observer,
    )?;
    let (reasoning, provider_options) = profile_request_options(compiled.profile)?;
    let bundle = run_tool_factory
        .compile(session.environment())
        .map_err(|source| {
            if source.kind() == RunToolFactoryErrorKind::WorkingDirectoryUnavailable
                && let Some(workspace_id) = session.environment().workspace_id.clone()
            {
                return RuntimeError::WorkspaceUnavailable { workspace_id };
            }
            RuntimeError::RunToolsBuildFailed { source }
        })?;
    let (tools, authorizer) = bundle.into_parts();

    let agent = AgentBuilder::new(
        compiled.model,
        session.system_prompt().clone(),
        context_window,
    )
    .tools(tools)
    .model_request(agent_core::ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: model_config.generation().clone(),
        reasoning,
        provider_options,
    })
    .budget(active.budget().clone())
    .build()
    .map_err(|source| RuntimeError::AgentBuildFailed { source })?;
    Ok(CompiledRunAgent { agent, authorizer })
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
