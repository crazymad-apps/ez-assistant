//! Run 与连接验证共用的模型服务编译。

use std::{sync::Arc, time::Duration};

use agent_model::{ModelService, ProviderOptions, ReasoningConfig, RetryingModelService};
use agent_sdk::{Agent, AgentBuilder};
use agent_types::ToolChoice;
use assistant_protocol::ModelKey;

use super::AssistantRuntime;
use crate::{
    ModelCompatibilityProfile, ModelServiceFactoryRequest, RuntimeError, RuntimeResult,
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

impl AssistantRuntime {
    /// 使用一次配置快照和 Session 冻结的 System Prompt 构造本 Run 独享的 Agent。
    pub(super) fn compile_run_agent(
        &self,
        session: &SessionController,
        snapshot: &ConfigSnapshot,
    ) -> RuntimeResult<Agent> {
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        let model_config = resolve_model(snapshot, session.model_key())?;
        let compiled = self.compile_model_service(snapshot, session.model_key())?;
        let (reasoning, provider_options) = profile_request_options(compiled.profile)?;

        AgentBuilder::new(
            compiled.model,
            session.system_prompt().clone(),
            self.context_window.clone(),
        )
        .tools(self.tools.clone())
        .model_request(agent_core::ModelRequestConfig {
            tool_choice: ToolChoice::Auto,
            generation: model_config.generation().clone(),
            reasoning,
            provider_options,
        })
        .budget(active.budget().clone())
        .build()
        .map_err(|source| RuntimeError::AgentBuildFailed { source })
    }

    /// 从同一配置快照构造 Run 和连接验证共用的冻结 ModelService。
    pub(super) fn compile_model_service(
        &self,
        snapshot: &ConfigSnapshot,
        model_key: &assistant_protocol::ModelKey,
    ) -> RuntimeResult<CompiledModelService> {
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        let model_config = resolve_model(snapshot, model_key)?;
        let transport = active.transport();
        let profile = model_config.compatibility_profile();
        let base_model = self
            .model_factory
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
        let model = active.retry_policy().map_or(base_model.clone(), |policy| {
            Arc::new(RetryingModelService::new(base_model, policy.clone())) as Arc<dyn ModelService>
        });
        Ok(CompiledModelService {
            model,
            profile,
            max_output_tokens: model_config.max_output_tokens(),
            request_timeout: transport.request_timeout(),
        })
    }
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
