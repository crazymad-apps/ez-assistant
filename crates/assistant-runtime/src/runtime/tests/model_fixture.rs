//! 显式构造易失 Store 中的新模型配置；与全局 TOML 完全独立，不解析旧字段。
use super::*;
use assistant_protocol::*;

pub(super) fn provider(api_key: &str) -> crate::StoredProvider {
    crate::StoredProvider {
        provider_instance_id: test_model_selection("fixture").provider_instance_id,
        connection: ProviderConnection {
            display_name: "Runtime test provider".into(),
            provider_type: ProviderType::Local,
            endpoint: "https://api.example.test/v1".into(),
            protocol_preference: ProviderProtocolPreference::ChatCompletions,
            models_path: "/v1/models".into(),
            discovery_format: ModelDiscoveryFormat::OpenAi,
        },
        api_key: SecretValue::new(api_key.into()),
    }
}

pub(super) fn parameters() -> ModelParameters {
    let supported = ModelFeatureSupport::Supported;
    ModelParameters {
        context_window_tokens: ModelTokenLimit::Known(8192.try_into().unwrap()),
        max_output_tokens: ModelTokenLimit::Known(4096.try_into().unwrap()),
        streaming: supported,
        tool_calls: supported,
        tool_choice: ModelToolChoiceSupport {
            auto: supported,
            none: supported,
            required: supported,
            named: supported,
        },
        image_input: ModelFeatureSupport::Unsupported,
        reasoning: ModelFeatureSupport::Unsupported,
        reasoning_mode: ModelReasoningMode::Unsupported,
        tool_image_projection: ModelToolImageProjection::Unsupported,
        ..Default::default()
    }
}

pub(super) async fn save_fixed(
    runtime: &AssistantRuntime,
    model_id: &str,
    parameters: ModelParameters,
) {
    runtime
        .store
        .put_model_fixed_config(ModelFixedConfig {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: test_model_selection(model_id),
            parameters,
            updated_at_ms: 1,
        })
        .await
        .unwrap();
}

pub(super) async fn seed(runtime: &AssistantRuntime, api_key: &str) {
    runtime.store.put_provider(provider(api_key)).await.unwrap();
    save_fixed(runtime, "fixture", parameters()).await;
    runtime
        .store
        .save_model_settings(ModelSettings {
            default_model: Some(test_model_selection("fixture")),
            vision_model: None,
        })
        .await
        .unwrap();
    runtime.restore_model_settings().await.unwrap();
}

/// 同步构造器仅用于未启动任务的易失 Store，要求每个初始化 future 立即完成；不阻塞 Tokio。
pub(super) fn seed_immediate(runtime: &AssistantRuntime) {
    use futures_util::FutureExt;
    seed(runtime, "unique-test-secret-9f1ca2")
        .now_or_never()
        .expect("new volatile fixture must not wait");
}

pub(super) fn discovery() -> crate::ModelDiscoveryFuture<'static> {
    Box::pin(async {
        Ok(["fixture", "alternate", "secondary"]
            .into_iter()
            .map(|model_id| crate::DiscoveredModel {
                configuration: None,
                model_id: model_id.into(),
                display_name: None,
                metadata: parameters(),
            })
            .collect())
    })
}
