use super::*;
use assistant_protocol::{
    ModelDiscoveryFormat, ModelToolChoiceSupport, ProviderInstanceId, ReasoningEffortKey,
    SecretValue,
};

fn known_limit(value: u64) -> ModelTokenLimit {
    ModelTokenLimit::Known(value.try_into().unwrap())
}
fn provider(kind: ProviderType, preference: ProviderProtocolPreference) -> StoredProvider {
    StoredProvider {
        server_history_key: None,
        provider_instance_id: ProviderInstanceId::new("provider-1").unwrap(),
        connection: ProviderConnection {
            display_name: "测试".into(),
            provider_type: kind,
            endpoint: "https://example.test/v1".into(),
            protocol_preference: preference,
            models_path: "/v1/models".into(),
            discovery_format: ModelDiscoveryFormat::OpenAi,
            server_history_enabled: false,
            server_history_support_override: None,
        },
        api_key: SecretValue::new("private-test-key".into()),
        model_catalog: None,
        model_catalog_diagnostic: None,
    }
}
fn selection() -> ModelSelection {
    ModelSelection {
        provider_instance_id: ProviderInstanceId::new("provider-1").unwrap(),
        model_id: "arbitrary/model-name".into(),
    }
}
fn parameters() -> ModelParameters {
    ModelParameters {
        context_window_tokens: known_limit(32768),
        max_input_tokens: known_limit(24000),
        max_output_tokens: known_limit(4096),
        streaming: ModelFeatureSupport::Supported,
        image_input: ModelFeatureSupport::Unsupported,
        tool_calls: ModelFeatureSupport::Unsupported,
        reasoning: ModelFeatureSupport::Unsupported,
        ..Default::default()
    }
}

#[test]
fn unknown_optional_capabilities_do_not_block_text_execution() {
    let provider = provider(ProviderType::Openai, ProviderProtocolPreference::Auto);
    for field in ["image", "tools", "reasoning"] {
        let mut values = parameters();
        match field {
            "streaming" => values.streaming = ModelFeatureSupport::Unknown,
            "image" => values.image_input = ModelFeatureSupport::Unknown,
            "tools" => values.tool_calls = ModelFeatureSupport::Unknown,
            _ => values.reasoning = ModelFeatureSupport::Unknown,
        }
        assert!(
            compile_managed_model(
                &provider,
                &selection(),
                &values,
                &GenerationConfig::default()
            )
            .is_ok()
        );
    }
    let mut partial_reasoning = parameters();
    partial_reasoning.reasoning = ModelFeatureSupport::Supported;
    for mode in [ModelReasoningMode::Unknown, ModelReasoningMode::Optional] {
        partial_reasoning.reasoning_mode = mode;
        assert!(
            compile_managed_model(
                &provider,
                &selection(),
                &partial_reasoning,
                &GenerationConfig::default()
            )
            .is_ok()
        );
    }
    let resolved = compile_managed_model(
        &provider,
        &selection(),
        &parameters(),
        &GenerationConfig::default(),
    )
    .unwrap();
    assert_eq!(resolved.model(), "arbitrary/model-name");
    assert!(!resolved.capabilities().tool_calls);
    assert_eq!(resolved.max_input_tokens(), Some(24000));
    assert_eq!(resolved.api_key(), "private-test-key");
}

#[test]
fn reasoning_limits_replace_the_normal_mode_limits_and_request_budget_is_separate() {
    let provider = provider(ProviderType::Moonshot, ProviderProtocolPreference::Auto);
    let mut values = parameters();
    values.reasoning = ModelFeatureSupport::Supported;
    values.reasoning_mode = ModelReasoningMode::Always;
    values.reasoning_efforts = Some(
        [
            (ReasoningEffortKey::Low, "low".into()),
            (ReasoningEffortKey::High, "high".into()),
        ]
        .into(),
    );
    values.default_reasoning_effort = Some(ReasoningEffortKey::High);
    values.reasoning_max_input_tokens = known_limit(16000);
    values.reasoning_max_output_tokens = known_limit(8192);
    let generation = GenerationConfig {
        max_output_tokens: Some(6000),
        ..Default::default()
    };
    let resolved = compile_managed_model(&provider, &selection(), &values, &generation).unwrap();
    assert_eq!(resolved.context_window_tokens(), 32768);
    assert_eq!(resolved.max_input_tokens(), Some(16000));
    assert_eq!(resolved.max_output_tokens(), 8192);
    assert_eq!(resolved.generation().max_output_tokens, Some(6000));
    assert_eq!(
        resolved.capabilities().reasoning.as_ref().unwrap().mode,
        ModelReasoningMode::Always
    );
    values.reasoning_efforts = None;
    values.default_reasoning_effort = None;
    let without_effort =
        compile_managed_model(&provider, &selection(), &values, &generation).unwrap();
    let reasoning = without_effort.capabilities().reasoning.as_ref().unwrap();
    assert_eq!(reasoning.mode, ModelReasoningMode::Always);
    assert!(reasoning.efforts.is_empty());
    assert_eq!(without_effort.max_input_tokens(), Some(16000));
}

#[test]
fn protocol_and_effort_encoding_do_not_depend_on_model_name() {
    for kind in [
        ProviderType::Openai,
        ProviderType::Deepseek,
        ProviderType::DashscopeApi,
        ProviderType::Moonshot,
        ProviderType::Zhipu,
        ProviderType::Vllm,
        ProviderType::Local,
    ] {
        let provider = provider(kind, ProviderProtocolPreference::Auto);
        let mut second = selection();
        second.model_id = "qwen3-thinking-deepseek-reasoner".into();
        let first = compile_managed_model(
            &provider,
            &selection(),
            &parameters(),
            &GenerationConfig::default(),
        )
        .unwrap();
        let renamed = compile_managed_model(
            &provider,
            &second,
            &parameters(),
            &GenerationConfig::default(),
        )
        .unwrap();
        assert_eq!(first.protocol(), renamed.protocol());
        assert_eq!(first.capabilities(), renamed.capabilities());
    }
    let invalid = provider(ProviderType::Vllm, ProviderProtocolPreference::Responses);
    assert!(
        compile_managed_model(
            &invalid,
            &selection(),
            &parameters(),
            &GenerationConfig::default()
        )
        .is_err()
    );
}

#[test]
fn tool_image_projection_and_deepseek_generation_must_be_encodable_by_the_provider() {
    let mut values = parameters();
    values.image_input = ModelFeatureSupport::Supported;
    values.tool_calls = ModelFeatureSupport::Supported;
    values.tool_choice = ModelToolChoiceSupport {
        auto: ModelFeatureSupport::Supported,
        none: ModelFeatureSupport::Supported,
        required: ModelFeatureSupport::Unsupported,
        named: ModelFeatureSupport::Unsupported,
    };
    let chat = provider(
        ProviderType::Openai,
        ProviderProtocolPreference::ChatCompletions,
    );
    let unknown_projection =
        compile_managed_model(&chat, &selection(), &values, &GenerationConfig::default()).unwrap();
    assert_eq!(
        unknown_projection.capabilities().tool_image_projection,
        ToolImageProjection::Unsupported
    );
    values.tool_choice.named = ModelFeatureSupport::Unknown;
    values.tool_choice.required = ModelFeatureSupport::Unknown;
    assert!(
        compile_managed_model(&chat, &selection(), &values, &GenerationConfig::default()).is_ok()
    );
    values.tool_image_projection = ModelToolImageProjection::NativeToolResult;
    assert!(
        compile_managed_model(&chat, &selection(), &values, &GenerationConfig::default()).is_err()
    );
    let responses = provider(ProviderType::Openai, ProviderProtocolPreference::Responses);
    assert!(
        compile_managed_model(
            &responses,
            &selection(),
            &values,
            &GenerationConfig::default()
        )
        .is_ok()
    );
    values.tool_image_projection = ModelToolImageProjection::FollowUpUserMessage;
    assert!(
        compile_managed_model(&chat, &selection(), &values, &GenerationConfig::default()).is_ok()
    );
    values.reasoning = ModelFeatureSupport::Supported;
    values.reasoning_mode = ModelReasoningMode::Optional;
    values.reasoning_efforts = Some(Default::default());
    let deepseek = provider(
        ProviderType::Deepseek,
        ProviderProtocolPreference::ChatCompletions,
    );
    assert!(
        compile_managed_model(
            &deepseek,
            &selection(),
            &values,
            &GenerationConfig {
                temperature: Some(0.5),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn restored_history_policy_requires_supported_responses_and_a_durable_binding() {
    let mut provider = provider(ProviderType::Openai, ProviderProtocolPreference::Responses);
    provider.connection.server_history_enabled = true;
    provider.server_history_key = Some("restored-binding".into());
    let compile = |provider: &StoredProvider| {
        compile_managed_model(
            provider,
            &selection(),
            &parameters(),
            &GenerationConfig::default(),
        )
    };
    assert!(
        compile(&provider).is_err(),
        "unknown custom endpoint remains closed after restore"
    );
    provider.connection.server_history_support_override = Some(ModelFeatureSupport::Supported);
    let resolved = compile(&provider).unwrap();
    assert_eq!(
        resolved.server_history_key.as_deref(),
        Some("restored-binding")
    );
    provider.server_history_key = None;
    assert!(
        compile(&provider).is_err(),
        "enabled connection needs a reliable activation binding"
    );
    provider.server_history_key = Some("restored-binding".into());
    provider.connection.protocol_preference = ProviderProtocolPreference::ChatCompletions;
    assert!(
        compile(&provider).is_err(),
        "restored Chat cannot use Responses history"
    );
}

#[test]
fn server_compaction_switches_are_independent_and_require_verified_responses() {
    let mut values = parameters();
    let mut responses = provider(ProviderType::Openai, ProviderProtocolPreference::Responses);
    responses.server_history_key = Some("compaction-binding".into());
    for support in [
        ModelFeatureSupport::Unknown,
        ModelFeatureSupport::Unsupported,
    ] {
        values.server_compaction_enabled = true;
        values.server_compaction_support = support;
        assert!(
            compile_managed_model(
                &responses,
                &selection(),
                &values,
                &GenerationConfig::default()
            )
            .is_err()
        );
    }
    values.server_compaction_support = ModelFeatureSupport::Supported;
    let compiled = compile_managed_model(
        &responses,
        &selection(),
        &values,
        &GenerationConfig::default(),
    )
    .unwrap();
    assert_eq!(
        compiled.context_management_mode(),
        agent_model::ContextManagementMode::ServerCompaction
    );
    assert!(
        !responses.connection.server_history_enabled,
        "compact does not require remote history"
    );
    assert!(
        compile_managed_model(
            &provider(
                ProviderType::Openai,
                ProviderProtocolPreference::ChatCompletions
            ),
            &selection(),
            &values,
            &GenerationConfig::default()
        )
        .is_err()
    );
    values.server_auto_compaction_enabled = true;
    assert!(
        compile_managed_model(
            &responses,
            &selection(),
            &values,
            &GenerationConfig::default()
        )
        .is_err()
    );
    values.server_compaction_enabled = false;
    assert!(
        compile_managed_model(
            &responses,
            &selection(),
            &values,
            &GenerationConfig::default()
        )
        .is_err()
    );
    values.server_auto_compaction_enabled = false;
    assert_eq!(
        compile_managed_model(
            &responses,
            &selection(),
            &values,
            &GenerationConfig::default()
        )
        .unwrap()
        .context_management_mode(),
        agent_model::ContextManagementMode::LocalSummary
    );
}

#[test]
fn opencode_requires_explicit_protocol_and_preserves_provider_identity() {
    let mut stored = provider(ProviderType::Opencode, ProviderProtocolPreference::Auto);
    assert!(super::super::validation::validate_connection(&stored.connection).is_err());
    for (preference, protocol) in [
        (
            ProviderProtocolPreference::ChatCompletions,
            ModelProtocol::OpenAiChatCompletions,
        ),
        (
            ProviderProtocolPreference::Responses,
            ModelProtocol::OpenAiResponses,
        ),
    ] {
        stored.connection.protocol_preference = preference;
        super::super::validation::validate_connection(&stored.connection).unwrap();
        let compiled = compile_managed_model(
            &stored,
            &selection(),
            &parameters(),
            &GenerationConfig::default(),
        )
        .unwrap();
        assert_eq!(compiled.protocol(), protocol);
        assert_eq!(compiled.provider().as_str(), "opencode");
        assert!(!compiled.server_history_enabled);
        assert!(!compiled.input_token_count_supported);
        let mut auxiliary_endpoints = parameters();
        auxiliary_endpoints.input_token_count_support = ModelFeatureSupport::Supported;
        assert!(
            compile_managed_model(
                &stored,
                &selection(),
                &auxiliary_endpoints,
                &GenerationConfig::default()
            )
            .is_err()
        );
        auxiliary_endpoints.input_token_count_support = ModelFeatureSupport::Unknown;
        auxiliary_endpoints.server_compaction_support = ModelFeatureSupport::Supported;
        auxiliary_endpoints.server_compaction_enabled = true;
        assert!(
            compile_managed_model(
                &stored,
                &selection(),
                &auxiliary_endpoints,
                &GenerationConfig::default()
            )
            .is_err()
        );

        stored.connection.server_history_enabled = true;
        assert!(super::super::validation::validate_connection(&stored.connection).is_err());
        stored.connection.server_history_enabled = false;
    }
    stored.connection.server_history_support_override = Some(ModelFeatureSupport::Supported);
    assert!(super::super::validation::validate_connection(&stored.connection).is_err());
    assert!(serde_json::from_str::<ProviderType>("\"future-provider\"").is_err());
}
