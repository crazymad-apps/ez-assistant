//! 服务商连接与生效参数到唯一执行配置的纯编译；无模型名分支、静态目录或网络回退。

use agent_model::{GenerationConfig, ToolChoiceCapabilities, ToolImageProjection};
use agent_types::ProviderId;
use assistant_protocol::{
    ModelFeatureSupport, ModelParameters, ModelReasoningMode, ModelSelection, ModelTokenLimit,
    ModelToolImageProjection, ProviderConnection, ProviderProtocolPreference, ProviderType,
};

use super::{
    ModelProtocol, ReasoningEffortWireValue, ResolvedModelCapabilities, ResolvedModelConfig,
    ResolvedReasoningCapability, ResolvedReasoningEffort, domain::ModelSecret,
};
use crate::{RuntimeError, RuntimeResult, StoredProvider};
use std::sync::Arc;

/// 一次执行准备冻结的模型与接纳依据；不注册第二份配置状态，也不缓存在线目录。
pub(crate) struct PreparedModel {
    pub(crate) selection: ModelSelection,
    pub(crate) model: ResolvedModelConfig,
    provider: Arc<StoredProvider>,
    follows_default: bool,
    active: Arc<super::ResolvedConfig>,
}

impl PreparedModel {
    /// 主／辅助模型准备可能跨多个 await，调用方在最终短接纳阶段再次核验全部准备值。
    pub(crate) fn ensure_current(&self, registry: &super::ConfigRegistry) -> RuntimeResult<()> {
        if !registry
            .snapshot()?
            .active()
            .is_some_and(|active| Arc::ptr_eq(active, &self.active))
        {
            return Err(RuntimeError::ConfigurationConflict);
        }
        let current = registry.managed_models()?;
        if !current
            .providers
            .get(&self.provider.provider_instance_id)
            .is_some_and(|provider| Arc::ptr_eq(provider, &self.provider))
            || (self.follows_default
                && current.settings.default_model.as_ref() != Some(&self.selection))
        {
            return Err(RuntimeError::ConfigurationConflict);
        }
        Ok(())
    }
}

impl super::ConfigRegistry {
    /// 旁路操作只读取已有固定参数；不进行在线发现，缺配置由用户重新选择或补齐。
    pub(crate) async fn configured_model(
        &self,
        snapshot: &super::ConfigSnapshot,
        requested: Option<&ModelSelection>,
        store: &dyn crate::RuntimeStore,
    ) -> RuntimeResult<PreparedModel> {
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        let (selection, provider) = {
            let models = self.managed_models()?;
            let selection = requested
                .or(models.settings.default_model.as_ref())
                .cloned()
                .ok_or_else(|| invalid("当前会话没有可用模型，请手动选择模型。"))?;
            let provider = models
                .providers
                .get(&selection.provider_instance_id)
                .cloned()
                .ok_or_else(|| invalid("所选服务商已删除，请手动重新选择模型。"))?;
            (selection, provider)
        };
        let fixed = store
            .get_model_fixed_config(selection.clone())
            .await
            .map_err(|e| RuntimeError::from_store("load configured model", e))?
            .ok_or_else(|| {
                invalid("当前会话没有可复用的模型参数，请重新选择模型或保存固定配置。")
            })?;
        let prepared = PreparedModel {
            model: compile_managed_model(
                &provider,
                &selection,
                &fixed.parameters,
                active.generation(),
            )?,
            selection,
            provider,
            follows_default: requested.is_none(),
            active: active.clone(),
        };
        prepared.ensure_current(self)?;
        Ok(prepared)
    }

    /// 网络仅消费已捕获连接；固定配置存在时普通执行不访问目录，且绝不混入在线字段。
    /// 无固定记录才在线确认身份；无显式引用的执行捕获当前默认。
    /// 返回前核验连接／固定值和跟随默认关系，最终接纳仍需调用 ensure_current。
    pub(crate) async fn prepare_model(
        &self,
        snapshot: &super::ConfigSnapshot,
        requested: Option<&ModelSelection>,
        store: &dyn crate::RuntimeStore,
        factory: &dyn crate::ModelServiceFactory,
    ) -> RuntimeResult<PreparedModel> {
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        let (selection, provider) = {
            let settings = self.managed_models()?;
            let selection = requested
                .or(settings.settings.default_model.as_ref())
                .cloned()
                .ok_or_else(|| invalid("尚未选择模型，请配置默认模型或为当前会话选择模型。"))?;
            let provider = settings
                .providers
                .get(&selection.provider_instance_id)
                .cloned()
                .ok_or_else(|| invalid("所选服务商已删除，请重新选择模型。"))?;
            (selection, provider)
        };
        let fetch = || async {
            let models = factory
                .discover_models(crate::ModelDiscoveryRequest {
                    format: provider.connection.discovery_format,
                    models_path: &provider.connection.models_path,
                    endpoint: &provider.connection.endpoint,
                    api_key: provider.api_key.expose(),
                    connect_timeout: active.transport().connect_timeout(),
                    request_timeout: active.transport().request_timeout(),
                })
                .await
                .map_err(|_| invalid("获取在线模型列表失败，请重试或编辑已有固定配置。"))?;
            models
                .into_iter()
                .find(|model| model.model_id == selection.model_id)
                .ok_or_else(|| invalid("本次在线列表中没有所选模型，请重新选择。"))
        };
        let parameters = match store
            .get_model_fixed_config(selection.clone())
            .await
            .map_err(|e| RuntimeError::from_store("load model execution parameters", e))?
        {
            Some(fixed) => fixed.parameters,
            None => {
                let model = fetch().await?;
                super::model_templates::configuration_detail(
                    &provider.connection,
                    selection.clone(),
                    model.metadata,
                )
                .parameters
            }
        };
        let model = compile_managed_model(&provider, &selection, &parameters, active.generation())?;
        let prepared = PreparedModel {
            selection,
            provider,
            model,
            follows_default: requested.is_none(),
            active: active.clone(),
        };
        prepared.ensure_current(self)?;
        Ok(prepared)
    }
}

/// 根据已确认的实例接入规则决定协议；推理失败不得切换协议重发。
pub(crate) fn resolve_provider_protocol(
    connection: &ProviderConnection,
) -> RuntimeResult<ModelProtocol> {
    use ProviderProtocolPreference::*;
    let responses = matches!(
        connection.provider_type,
        ProviderType::Openai
            | ProviderType::Deepseek
            | ProviderType::DashscopeApi
            | ProviderType::DashscopePlan
            | ProviderType::Moonshot
    );
    match connection.protocol_preference {
        Responses if !responses => Err(invalid("该服务商未声明支持 Responses 协议。")),
        Responses => Ok(ModelProtocol::OpenAiResponses),
        Auto if responses => Ok(ModelProtocol::OpenAiResponses),
        Auto | ChatCompletions => Ok(ModelProtocol::OpenAiChatCompletions),
    }
}

/// 基础文本执行只要求窗口、输出上限与流式能力；未知可选能力不启用，原始参数仍保留未知。
/// 输入和输出独立限制按实际思考模式选取，global_generation 只约束请求预算，不覆盖模型事实。
pub(crate) fn compile_managed_model(
    provider: &StoredProvider,
    selection: &ModelSelection,
    parameters: &ModelParameters,
    global_generation: &GenerationConfig,
) -> RuntimeResult<ResolvedModelConfig> {
    if provider.provider_instance_id != selection.provider_instance_id {
        return Err(invalid("模型选择与服务商实例不匹配。"));
    }
    let validated = crate::validate_fixed_model_parameters(parameters)
        .map_err(|_| invalid("模型参数不完整或数值、能力组合无效，请编辑固定配置。"))?;
    let protocol = resolve_provider_protocol(&provider.connection)?;
    let image_input = parameters.image_input == ModelFeatureSupport::Supported;
    let tool_calls = parameters.tool_calls == ModelFeatureSupport::Supported
        && parameters.tool_choice.auto == ModelFeatureSupport::Supported;
    if !known(parameters.streaming, "流式输出能力未知，请编辑固定配置。")? {
        return Err(invalid("当前执行要求模型支持流式输出。"));
    }
    let reasoning = if parameters.reasoning == ModelFeatureSupport::Supported
        && matches!(
            parameters.reasoning_mode,
            ModelReasoningMode::Optional | ModelReasoningMode::Always
        ) {
        let efforts = parameters
            .reasoning_efforts
            .iter()
            .flat_map(|efforts| efforts.iter());
        Some(ResolvedReasoningCapability {
            mode: parameters.reasoning_mode,
            efforts: efforts
                .map(|(key, wire_value)| ResolvedReasoningEffort {
                    key: *key,
                    label: key.as_str().to_owned(),
                    wire_value: ReasoningEffortWireValue::String(wire_value.clone()),
                })
                .collect(),
            default_effort: parameters.default_reasoning_effort,
        })
    } else {
        None
    };
    let tool_choice = if tool_calls {
        ToolChoiceCapabilities {
            auto: parameters.tool_choice.auto == ModelFeatureSupport::Supported,
            none: parameters.tool_choice.none == ModelFeatureSupport::Supported,
            required: parameters.tool_choice.required == ModelFeatureSupport::Supported,
            named: parameters.tool_choice.named == ModelFeatureSupport::Supported,
        }
    } else {
        ToolChoiceCapabilities::default()
    };
    let tool_image_projection = match parameters.tool_image_projection {
        ModelToolImageProjection::NativeToolResult
            if protocol == ModelProtocol::OpenAiResponses =>
        {
            ToolImageProjection::NativeFunctionOutput
        }
        ModelToolImageProjection::NativeToolResult => {
            return Err(invalid("Chat Completions 不支持工具结果原生图片。"));
        }
        ModelToolImageProjection::FollowUpUserMessage => ToolImageProjection::AggregatedUserInput,
        ModelToolImageProjection::Unsupported => ToolImageProjection::Unsupported,
        ModelToolImageProjection::Unknown => ToolImageProjection::Unsupported,
    };
    let provider_name = match provider.connection.provider_type {
        ProviderType::Openai => "openai",
        ProviderType::Deepseek => "deepseek",
        ProviderType::DashscopeApi => "dashscope_api",
        ProviderType::DashscopePlan => "dashscope_plan",
        ProviderType::Moonshot => "moonshot",
        ProviderType::Zhipu => "zhipu",
        ProviderType::Vllm => "vllm",
        ProviderType::Local => "local",
    };
    if provider.connection.provider_type == ProviderType::Deepseek
        && reasoning.is_some()
        && (global_generation.temperature.is_some() || global_generation.top_p.is_some())
    {
        return Err(invalid(
            "DeepSeek 思考模式不支持已配置的 temperature 或 top_p。",
        ));
    }
    let input = if reasoning.is_some() {
        optional(parameters.reasoning_max_input_tokens)
            .or_else(|| optional(parameters.max_input_tokens))
    } else {
        optional(parameters.max_input_tokens)
    };
    let output = if reasoning.is_some() {
        optional(parameters.reasoning_max_output_tokens)
            .unwrap_or(u64::from(validated.max_output_tokens()))
    } else {
        u64::from(validated.max_output_tokens())
    };
    let output = u32::try_from(output).map_err(|_| invalid("模型输出上限超出支持范围。"))?;
    let mut generation = global_generation.clone();
    generation.max_output_tokens = Some(
        generation
            .max_output_tokens
            .map_or(output, |budget| budget.min(output)),
    );
    Ok(ResolvedModelConfig {
        display_name: selection.model_id.clone(),
        protocol,
        provider: ProviderId::new(provider_name.to_owned())
            .map_err(|_| invalid("服务商类型无效。"))?,
        endpoint: provider.connection.endpoint.clone(),
        model: selection.model_id.clone(),
        api_key: ModelSecret::new(provider.api_key.expose().to_owned()),
        context_window_tokens: validated.context_window_tokens(),
        max_input_tokens: input,
        max_output_tokens: output,
        generation,
        capabilities: ResolvedModelCapabilities {
            image_input,
            tool_calls,
            reasoning,
            streaming: true,
            tool_choice,
            tool_image_projection,
        },
    })
}

fn known(value: ModelFeatureSupport, missing: &'static str) -> RuntimeResult<bool> {
    match value {
        ModelFeatureSupport::Supported => Ok(true),
        ModelFeatureSupport::Unsupported => Ok(false),
        ModelFeatureSupport::Unknown => Err(invalid(missing)),
    }
}
fn optional(value: ModelTokenLimit) -> Option<u64> {
    match value {
        ModelTokenLimit::Known(value) => Some(value.get()),
        _ => None,
    }
}
fn invalid(reason: &'static str) -> RuntimeError {
    RuntimeError::InvalidRequest { reason }
}

#[cfg(test)]
mod tests;
