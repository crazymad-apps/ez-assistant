//! 已编译 Runtime 模型配置到 OpenAI-compatible Adapter 的 Host 装配。

mod discovery;

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
    fn discover_models<'a>(
        &'a self,
        request: assistant_runtime::ModelDiscoveryRequest<'a>,
    ) -> assistant_runtime::ModelDiscoveryFuture<'a> {
        Box::pin(discovery::discover(request))
    }

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
                let mut adapter =
                    chat_adapter(request.provider, request.capabilities.reasoning_enabled());
                if request.capabilities.reasoning.is_some() {
                    let effort_field = (!effort_values.is_empty()).then_some("reasoning_effort");
                    adapter = if adapter.supports_reasoning() {
                        adapter.with_reasoning_efforts(effort_field, effort_values)
                    } else {
                        adapter.with_reasoning(
                            Some("reasoning_content"),
                            effort_field,
                            effort_values,
                        )
                    };
                    if let Some(policy) = reasoning_replay_policy(request.provider) {
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
                    .map_err(model_service_error)?
                    .with_max_input_tokens(request.max_input_tokens)
                    .map_err(model_service_error)?,
                )
            }
            ModelProtocol::OpenAiResponses => {
                let mut adapter = responses_adapter(request.provider)
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
                    .map_err(model_service_error)?
                    .with_max_input_tokens(request.max_input_tokens)
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

/// 根据显式 Provider 身份选择 Chat Completions wire 方言。
///
/// `local` 仍是普通 OpenAI-compatible；只有 `provider = "vllm"` 才启用 vLLM 当前
/// `reasoning` 字段及旧 `reasoning_content` 响应兼容，不能根据 loopback endpoint 猜测。
fn chat_adapter(
    provider: &agent_types::ProviderId,
    reasoning_enabled: bool,
) -> ChatProtocolAdapter {
    match (provider.as_str(), reasoning_enabled) {
        ("deepseek", true) => ChatProtocolAdapter::deepseek(),
        ("vllm", true) => ChatProtocolAdapter::vllm(),
        _ => ChatProtocolAdapter::openai_compatible(provider.clone()),
    }
}

fn responses_adapter(provider: &agent_types::ProviderId) -> ResponsesProtocolAdapter {
    match provider.as_str() {
        "deepseek" => ResponsesProtocolAdapter::deepseek(),
        "dashscope_api" | "dashscope_plan" => ResponsesProtocolAdapter::qwen(),
        "moonshot" => ResponsesProtocolAdapter::kimi(),
        "openai" => ResponsesProtocolAdapter::openai(),
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

/// 思考历史编码属于服务商方言；是否思考由生效模型能力决定，不按模型 ID 分支。
fn reasoning_replay_policy(provider: &agent_types::ProviderId) -> Option<ReasoningReplayPolicy> {
    match provider.as_str() {
        "dashscope_api" | "dashscope_plan" | "moonshot" => Some(ReasoningReplayPolicy::PreserveAll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use agent_types::ProviderId;
    #[test]
    fn both_protocols_bind_input_limits_without_changing_the_context_window() {
        use super::*;
        use assistant_runtime::ResolvedModelCapabilities;
        let home = tempfile::tempdir().unwrap();
        let factory = HostModelServiceFactory::new(home.path());
        let provider = ProviderId::new("openai").unwrap();
        let capabilities = ResolvedModelCapabilities {
            image_input: false,
            reasoning: None,
            tool_calls: false,
            tool_image_projection: agent_model::ToolImageProjection::Unsupported,
            tool_choice: agent_model::ToolChoiceCapabilities::default(),
            streaming: true,
        };
        for protocol in [
            ModelProtocol::OpenAiChatCompletions,
            ModelProtocol::OpenAiResponses,
        ] {
            for limit in [None, Some(64000), Some(0), Some(128001)] {
                let result = factory.create_model(ModelServiceFactoryRequest {
                    provider: &provider,
                    protocol,
                    capabilities: &capabilities,
                    endpoint: "https://example.test/v1",
                    model: "org/model",
                    api_key: "artificial-key",
                    context_window_tokens: 128000,
                    max_input_tokens: limit,
                    connect_timeout: std::time::Duration::from_secs(1),
                    request_timeout: std::time::Duration::from_secs(1),
                });
                if matches!(limit, Some(0 | 128001)) {
                    assert!(result.is_err());
                    continue;
                }
                let bundle = result.unwrap();
                assert_eq!(bundle.model.context_window_tokens(), 128000);
                assert_eq!(bundle.model.max_input_tokens(), limit);
            }
        }
    }

    #[test]
    fn provider_rules_select_responses_and_reasoning_replay_without_model_identity() {
        use agent_openai_compatible::{ReasoningReplayPolicy, ResponsesProtocolAdapter};
        for (name, adapter, replay) in [
            ("deepseek", ResponsesProtocolAdapter::deepseek(), None),
            (
                "dashscope_api",
                ResponsesProtocolAdapter::qwen(),
                Some(ReasoningReplayPolicy::PreserveAll),
            ),
            (
                "moonshot",
                ResponsesProtocolAdapter::kimi(),
                Some(ReasoningReplayPolicy::PreserveAll),
            ),
            ("openai", ResponsesProtocolAdapter::openai(), None),
        ] {
            let provider = ProviderId::new(name).unwrap();
            assert_eq!(super::responses_adapter(&provider), adapter);
            assert_eq!(super::reasoning_replay_policy(&provider), replay);
        }
    }

    #[test]
    fn vllm_chat_dialect_requires_explicit_provider_and_reasoning() {
        let vllm = ProviderId::new("vllm").expect("provider id");
        let local = ProviderId::new("local").expect("provider id");
        assert_eq!(
            super::chat_adapter(&vllm, true),
            agent_openai_compatible::ChatProtocolAdapter::vllm()
        );
        assert_eq!(
            super::chat_adapter(&vllm, false),
            agent_openai_compatible::ChatProtocolAdapter::openai_compatible(vllm)
        );
        assert_eq!(
            super::chat_adapter(&local, true),
            agent_openai_compatible::ChatProtocolAdapter::openai_compatible(local)
        );
    }
}
