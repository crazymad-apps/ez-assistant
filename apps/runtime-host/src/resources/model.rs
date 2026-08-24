//! 已编译 Runtime 模型配置到 OpenAI-compatible Adapter 的 Host 装配。

use std::{collections::BTreeMap, path::Path, sync::Arc};

use agent_model::{ModelCapabilities, ModelService, ModelServiceBundle, ReasoningEffort};
use agent_openai_compatible::{
    BearerCredential, ChatProtocolAdapter, FunctionOutputShape, OpenAiChatCompletionsService,
    OpenAiResponsesService, ReasoningReplayPolicy, ResponsesProtocolAdapter, TransportTimeouts,
};
use assistant_runtime::{
    ModelProtocol, ModelServiceFactory, ModelServiceFactoryError, ModelServiceFactoryRequest,
    ReasoningEffortKey, ReasoningEffortWireValue,
};

use crate::image::HostModelImagePreprocessor;

pub(super) struct HostModelServiceFactory {
    image_preprocessor: Arc<HostModelImagePreprocessor>,
}

impl HostModelServiceFactory {
    pub(super) fn new(runtime_home: &Path) -> Self {
        Self {
            image_preprocessor: Arc::new(HostModelImagePreprocessor::new(runtime_home)),
        }
    }
}

impl ModelServiceFactory for HostModelServiceFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        let capabilities = ModelCapabilities {
            reasoning: request.capabilities.reasoning_enabled(),
            image_input: request.capabilities.image_input,
            tool_calls: request.capabilities.tool_calls,
            multimodal_tool_result: request.capabilities.tool_image_projection
                != agent_model::ToolImageProjection::Unsupported,
            tool_choice: request.capabilities.tool_choice,
            streaming: request.capabilities.streaming,
        };
        let timeouts = TransportTimeouts {
            connect: request.connect_timeout,
            request: request.request_timeout,
        };
        let effort_values = compile_effort_values(request.capabilities);
        let service: Arc<dyn ModelService> = match request.protocol {
            ModelProtocol::OpenAiChatCompletions => {
                let mut adapter = if request.provider.as_str() == "deepseek"
                    && request.capabilities.reasoning_enabled()
                {
                    ChatProtocolAdapter::deepseek()
                } else {
                    ChatProtocolAdapter::openai_compatible(request.provider.clone())
                };
                if request.capabilities.reasoning.is_some() {
                    let effort_field = (!effort_values.is_empty())
                        .then_some(reasoning_effort_field(request.provider, request.model));
                    adapter = adapter.with_reasoning(
                        Some("reasoning_content"),
                        effort_field,
                        effort_values,
                    );
                    if let Some(policy) = reasoning_replay_policy(request.provider, request.model) {
                        adapter = adapter.with_reasoning_replay(policy);
                    }
                }
                adapter =
                    adapter.with_tool_image_projection(request.capabilities.tool_image_projection);
                Arc::new(
                    OpenAiChatCompletionsService::new_with_capabilities(
                        request.endpoint,
                        BearerCredential::new(request.api_key.to_owned()),
                        request.model,
                        request.context_window_tokens,
                        adapter,
                        capabilities,
                        timeouts,
                    )
                    .map_err(model_service_error)?,
                )
            }
            ModelProtocol::OpenAiResponses => {
                let mut adapter = responses_adapter(request.provider, request.model)
                    .with_reasoning_efforts(effort_values)
                    .with_tool_choice(request.capabilities.tool_choice)
                    .with_tool_image_projection(request.capabilities.tool_image_projection);
                if request.capabilities.tool_image_projection
                    == agent_model::ToolImageProjection::NativeFunctionOutput
                {
                    adapter = adapter.with_function_output_shape(FunctionOutputShape::ContentParts);
                }
                Arc::new(
                    OpenAiResponsesService::new_with_capabilities(
                        request.endpoint,
                        BearerCredential::new(request.api_key.to_owned()),
                        request.model,
                        request.context_window_tokens,
                        adapter,
                        capabilities,
                        timeouts,
                    )
                    .map_err(model_service_error)?,
                )
            }
        };
        if request.capabilities.image_input {
            Ok(ModelServiceBundle::with_image_preprocessor(
                service,
                self.image_preprocessor.clone(),
            ))
        } else {
            Ok(ModelServiceBundle::text_only(service))
        }
    }
}

fn responses_adapter(
    provider: &agent_types::ProviderId,
    model_id: &str,
) -> ResponsesProtocolAdapter {
    match (provider.as_str(), model_id) {
        ("deepseek", "deepseek-v4-flash" | "deepseek-v4-pro" | "deepseek-v4-flash-vision-exp") => {
            ResponsesProtocolAdapter::deepseek()
        }
        ("dashscope", "qwen3.8-max") => ResponsesProtocolAdapter::qwen(),
        ("moonshot", "k3") => ResponsesProtocolAdapter::kimi(),
        ("openai", _) => ResponsesProtocolAdapter::openai(),
        _ => ResponsesProtocolAdapter::openai_compatible(provider.clone()),
    }
}

fn compile_effort_values(
    capabilities: &assistant_runtime::ResolvedModelCapabilities,
) -> BTreeMap<ReasoningEffort, serde_json::Value> {
    capabilities
        .reasoning
        .iter()
        .flat_map(|reasoning| &reasoning.efforts)
        .map(|effort| {
            let key = match effort.key {
                ReasoningEffortKey::Low => ReasoningEffort::Low,
                ReasoningEffortKey::Medium => ReasoningEffort::Medium,
                ReasoningEffortKey::High => ReasoningEffort::High,
                ReasoningEffortKey::XHigh => ReasoningEffort::XHigh,
                ReasoningEffortKey::Max => ReasoningEffort::Max,
            };
            let value = match &effort.wire_value {
                ReasoningEffortWireValue::String(value) => serde_json::Value::String(value.clone()),
                ReasoningEffortWireValue::PositiveInteger(value) => serde_json::Value::from(*value),
            };
            (key, value)
        })
        .collect()
}

fn model_service_error(
    source: impl std::error::Error + Send + Sync + 'static,
) -> ModelServiceFactoryError {
    ModelServiceFactoryError::with_source("model service could not be created", source)
}

/// effort 字段属于具体服务方言与模型批次，不能只按 OpenAI-compatible 协议猜测。
fn reasoning_effort_field(provider: &agent_types::ProviderId, model_id: &str) -> &'static str {
    match (provider.as_str(), model_id) {
        ("dashscope", "qwen3.8-max") => "reasoning_effort",
        ("dashscope", _) => "thinking_budget",
        _ => "reasoning_effort",
    }
}

/// reasoning 历史回放属于具体模型批次的协议方言，不能由公共字段名推断。
fn reasoning_replay_policy(
    provider: &agent_types::ProviderId,
    model_id: &str,
) -> Option<ReasoningReplayPolicy> {
    match (provider.as_str(), model_id) {
        // Qwen 3.8 的 preserve_thinking 模式要求后续请求完整携带历史 reasoning_content。
        ("dashscope", "qwen3.8-max") => Some(ReasoningReplayPolicy::PreserveAll),
        // 公共 Kimi API 与 Kimi Code 使用不同 ID，但 K3 都要求保留历史 reasoning_content。
        ("moonshot", "kimi-k3" | "k3") => Some(ReasoningReplayPolicy::PreserveAll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use agent_types::ProviderId;
    use assistant_runtime::{ModelCatalog, ModelProtocol, ReasoningEffortKey};

    fn resolved(
        catalog: &ModelCatalog,
        provider: &str,
        model_id: &str,
    ) -> assistant_runtime::ResolvedModelCapabilities {
        resolved_protocol(
            catalog,
            provider,
            ModelProtocol::OpenAiChatCompletions,
            model_id,
        )
    }

    fn resolved_protocol(
        catalog: &ModelCatalog,
        provider: &str,
        protocol: ModelProtocol,
        model_id: &str,
    ) -> assistant_runtime::ResolvedModelCapabilities {
        catalog.resolve(
            &ProviderId::new(provider).expect("provider id"),
            protocol,
            model_id,
        )
    }

    #[test]
    fn bundled_model_catalog_is_strictly_valid() {
        ModelCatalog::from_json(include_str!("../../resources/model-catalog.json"))
            .expect("bundled model catalog");
    }

    #[test]
    fn bundled_model_catalog_contains_only_the_latest_verified_batches() {
        let catalog = ModelCatalog::from_json(include_str!("../../resources/model-catalog.json"))
            .expect("bundled model catalog");

        assert_eq!(catalog.revision(), "2026-08-22-m6");
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "dashscope"
                && route.provider_label == "阿里云百炼（Qwen）"
                && route.protocol_label == "Chat Completions（OpenAI Compatible）"
                && route.model_ids == ["qwen3.8-max"]
        }));
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "deepseek"
                && route.protocol == ModelProtocol::OpenAiResponses
                && route.model_ids == ["deepseek-v4-flash", "deepseek-v4-pro"]
        }));
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "deepseek"
                && route.protocol == ModelProtocol::OpenAiResponses
                && route.model_ids == ["deepseek-v4-flash-vision-exp"]
        }));
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "dashscope"
                && route.protocol == ModelProtocol::OpenAiResponses
                && route.model_ids == ["qwen3.8-max"]
        }));
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "moonshot"
                && route.protocol == ModelProtocol::OpenAiResponses
                && route.model_ids == ["k3"]
        }));

        let openai = resolved(&catalog, "openai", "gpt-5.6");
        assert!(openai.image_input);
        assert_eq!(
            openai.tool_image_projection,
            agent_model::ToolImageProjection::Unsupported
        );
        assert_eq!(
            openai
                .reasoning
                .expect("gpt-5.6 reasoning")
                .efforts
                .into_iter()
                .map(|effort| effort.key)
                .collect::<Vec<_>>(),
            [
                ReasoningEffortKey::Low,
                ReasoningEffortKey::Medium,
                ReasoningEffortKey::High,
                ReasoningEffortKey::XHigh,
                ReasoningEffortKey::Max,
            ]
        );

        let qwen = resolved(&catalog, "dashscope", "qwen3.8-max");
        assert!(qwen.image_input);
        assert_eq!(
            qwen.tool_image_projection,
            agent_model::ToolImageProjection::AggregatedUserInput
        );
        assert_eq!(
            qwen.reasoning
                .expect("qwen3.8-max reasoning")
                .default_effort,
            Some(ReasoningEffortKey::XHigh)
        );

        let kimi = resolved(&catalog, "moonshot", "kimi-k3");
        assert!(kimi.image_input);
        assert_eq!(
            kimi.tool_image_projection,
            agent_model::ToolImageProjection::AggregatedUserInput
        );
        assert_eq!(
            kimi.reasoning.expect("kimi-k3 reasoning").default_effort,
            Some(ReasoningEffortKey::Max)
        );

        let kimi_code = resolved(&catalog, "moonshot", "k3");
        assert!(kimi_code.image_input);
        assert_eq!(
            kimi_code.tool_image_projection,
            agent_model::ToolImageProjection::AggregatedUserInput
        );
        assert_eq!(
            kimi_code.reasoning.expect("k3 reasoning").default_effort,
            Some(ReasoningEffortKey::High)
        );
        assert!(catalog.routes().iter().any(|route| {
            route.provider.as_str() == "moonshot"
                && route.provider_label == "Moonshot（Kimi）"
                && route.model_ids == ["k3"]
        }));

        assert!(resolved(&catalog, "zhipu", "glm-5v-turbo").image_input);
        assert!(
            resolved(&catalog, "deepseek", "deepseek-v4-pro")
                .reasoning
                .is_some()
        );
        assert_eq!(
            resolved(&catalog, "deepseek", "deepseek-v4-pro").tool_image_projection,
            agent_model::ToolImageProjection::Unsupported
        );
        let deepseek_vision = resolved(&catalog, "deepseek", "deepseek-v4-flash-vision-exp");
        assert!(deepseek_vision.image_input);
        assert_eq!(
            deepseek_vision.tool_image_projection,
            agent_model::ToolImageProjection::AggregatedUserInput
        );
        assert_eq!(
            deepseek_vision
                .reasoning
                .as_ref()
                .expect("deepseek vision reasoning")
                .default_effort,
            Some(ReasoningEffortKey::High)
        );
        assert_eq!(
            deepseek_vision
                .reasoning
                .expect("deepseek vision reasoning")
                .efforts
                .into_iter()
                .map(|effort| effort.key)
                .collect::<Vec<_>>(),
            [ReasoningEffortKey::High, ReasoningEffortKey::Max]
        );

        // 旧批次不再由随包表猜测能力，未命中时回到协议保守基线。
        let legacy = resolved(&catalog, "deepseek", "deepseek-chat");
        assert!(!legacy.image_input);
        assert!(legacy.reasoning.is_none());

        let deepseek_responses = resolved_protocol(
            &catalog,
            "deepseek",
            ModelProtocol::OpenAiResponses,
            "deepseek-v4-pro",
        );
        assert!(deepseek_responses.tool_calls);
        assert!(deepseek_responses.reasoning_enabled());
        assert!(!deepseek_responses.image_input);
        let deepseek_vision_responses = resolved_protocol(
            &catalog,
            "deepseek",
            ModelProtocol::OpenAiResponses,
            "deepseek-v4-flash-vision-exp",
        );
        assert!(deepseek_vision_responses.image_input);
        assert_eq!(
            deepseek_vision_responses.tool_image_projection,
            agent_model::ToolImageProjection::NativeFunctionOutput
        );
        let qwen_responses = resolved_protocol(
            &catalog,
            "dashscope",
            ModelProtocol::OpenAiResponses,
            "qwen3.8-max",
        );
        assert!(qwen_responses.image_input);
        assert_eq!(
            qwen_responses.tool_image_projection,
            agent_model::ToolImageProjection::AggregatedUserInput
        );
        let kimi_responses =
            resolved_protocol(&catalog, "moonshot", ModelProtocol::OpenAiResponses, "k3");
        assert!(kimi_responses.image_input);
        assert_eq!(
            kimi_responses.tool_image_projection,
            agent_model::ToolImageProjection::NativeFunctionOutput
        );
        assert_eq!(
            resolved_protocol(&catalog, "zhipu", ModelProtocol::OpenAiResponses, "glm-5.2",),
            assistant_runtime::ResolvedModelCapabilities::conservative_openai_responses()
        );
    }

    #[test]
    fn qwen38_uses_its_documented_string_effort_field() {
        let dashscope = ProviderId::new("dashscope").expect("provider id");
        let moonshot = ProviderId::new("moonshot").expect("provider id");
        assert_eq!(
            super::reasoning_effort_field(&dashscope, "qwen3.8-max"),
            "reasoning_effort"
        );
        assert_eq!(
            super::reasoning_effort_field(&dashscope, "future-budget-model"),
            "thinking_budget"
        );
        assert_eq!(
            super::reasoning_replay_policy(&dashscope, "qwen3.8-max"),
            Some(agent_openai_compatible::ReasoningReplayPolicy::PreserveAll)
        );
        assert_eq!(
            super::reasoning_replay_policy(&dashscope, "future-budget-model"),
            None
        );
        for model_id in ["kimi-k3", "k3"] {
            assert_eq!(
                super::reasoning_replay_policy(&moonshot, model_id),
                Some(agent_openai_compatible::ReasoningReplayPolicy::PreserveAll)
            );
        }
    }

    #[test]
    fn deepseek_vision_responses_uses_the_verified_deepseek_dialect() {
        let deepseek = ProviderId::new("deepseek").expect("provider id");
        assert_eq!(
            super::responses_adapter(&deepseek, "deepseek-v4-flash-vision-exp"),
            agent_openai_compatible::ResponsesProtocolAdapter::deepseek()
        );
    }
}
