//! 外部模型状态只在 Runtime 内存生效；旧选择、旧 Arc 和本地 Store 均不能成为旁路。

use super::*;
use crate::{ExternalModelConfiguration, ModelSource};

struct ExternalFactory;

impl ModelServiceFactory for ExternalFactory {
    fn configuration_source(&self) -> ModelSource {
        ModelSource::External
    }

    fn create_model(
        &self,
        _: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        Ok(ModelServiceBundle::text_only(empty_model()))
    }
}

async fn fixture() -> AssistantRuntime {
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
        Arc::new(MissingConfigSource),
        Arc::new(ExternalFactory),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolRegistry::new().snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    // Store 刻意保留历史个人连接、默认与固定值，外部恢复必须忽略它们。
    model_fixture::seed(&runtime, "local-secret-must-not-be-used").await;
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
}

fn configuration(model: &str) -> Arc<ExternalModelConfiguration> {
    Arc::new(
        ExternalModelConfiguration::new(
            model_fixture::provider("").connection,
            model.to_owned(),
            model_fixture::parameters(),
            42,
        )
        .unwrap(),
    )
}

async fn publish(runtime: &AssistantRuntime, value: Arc<ExternalModelConfiguration>) {
    let mut gate = runtime.external_model_publication().await.unwrap();
    gate.publish(Some(value), "", 42).unwrap();
    gate.finish();
}

#[tokio::test]
async fn external_preparation_ignores_local_store_and_historical_selection() {
    let runtime = fixture().await;
    assert!(runtime.list_providers().unwrap().is_empty());
    let snapshot = runtime.config_registry.snapshot().unwrap();
    let historical = test_model_selection("fixture");
    assert!(matches!(
        runtime
            .config_registry
            .prepare_model(&snapshot, Some(&historical), runtime.store.as_ref())
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    publish(&runtime, configuration("managed-model")).await;
    let providers = runtime.list_providers().unwrap();
    assert_eq!(providers.len(), 1);
    let selection = runtime.get_model_settings().unwrap().default_model.unwrap();
    assert_eq!(
        selection.provider_instance_id,
        providers[0].provider_instance_id
    );
    let catalog = runtime
        .list_provider_models(selection.provider_instance_id.clone())
        .await
        .unwrap();
    assert_eq!(catalog.models[0].model_id, selection.model_id);
    let detail = runtime
        .get_model_configuration(assistant_protocol::GetModelConfigurationRequest {
            selection: selection.clone(),
            origin: assistant_protocol::ModelConfigOrigin::Online,
        })
        .await
        .unwrap();
    assert_eq!(detail.parameters, model_fixture::parameters());
    let fixed = runtime
        .list_fixed_model_configs(assistant_protocol::ListFixedModelConfigsRequest {
            provider_instance_id: selection.provider_instance_id,
            offset: 0,
            limit: 20,
        })
        .await
        .unwrap();
    assert_eq!(fixed.len(), 1);
    assert_eq!(fixed[0].parameters, detail.parameters);

    for configured in [false, true] {
        let prepared = if configured {
            runtime
                .config_registry
                .configured_model(&snapshot, Some(&historical), runtime.store.as_ref())
                .await
        } else {
            runtime
                .config_registry
                .prepare_model(&snapshot, Some(&historical), runtime.store.as_ref())
                .await
        }
        .unwrap();
        assert_eq!(prepared.selection.model_id, "managed-model");
        assert_eq!(prepared.selection.provider_instance_id.as_str(), "managed");
        assert!(prepared.model.api_key().is_empty());
        assert!(prepared.model.external_configuration.is_some());
    }
    let stored = runtime.store.load_model_settings().await.unwrap();
    assert_eq!(stored.default_model, Some(historical));
    assert!(stored.management.is_none());
    assert!(runtime.get_model_settings().unwrap().vision_model.is_none());
    assert!(runtime.ensure_model_editable().is_err());
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn replacement_invalidates_prepared_arc_and_old_rejection_cannot_clear_new_state() {
    let runtime = fixture().await;
    let old = configuration("same-model");
    publish(&runtime, old.clone()).await;
    let snapshot = runtime.config_registry.snapshot().unwrap();
    let prepared = runtime
        .config_registry
        .prepare_model(&snapshot, None, runtime.store.as_ref())
        .await
        .unwrap();
    // 内容完全相同仍是一次新的接纳结果。
    let current = configuration("same-model");
    publish(&runtime, current.clone()).await;
    assert!(matches!(
        prepared.ensure_current(&runtime.config_registry),
        Err(RuntimeError::ConfigurationConflict)
    ));
    let mut gate = runtime.external_model_publication().await.unwrap();
    assert!(!gate.invalidate(&old, "旧请求失效").unwrap());
    gate.refresh_failed("网络不可达").unwrap();
    gate.finish();
    assert_eq!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .unwrap()
            .model_id,
        "same-model"
    );
    let mut gate = runtime.external_model_publication().await.unwrap();
    assert!(gate.invalidate(&current, "配置已失效").unwrap());
    gate.refresh_failed("网络不可达").unwrap();
    gate.finish();
    assert!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .is_none()
    );
    assert!(matches!(
        runtime
            .config_registry
            .configured_model(&snapshot, None, runtime.store.as_ref())
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    publish(&runtime, configuration("new-model")).await;
    let mut gate = runtime.external_model_publication().await.unwrap();
    gate.publish(None, "管理员尚未配置模型", 43).unwrap();
    gate.finish();
    let status = runtime.get_model_settings().unwrap().management.unwrap();
    assert!(status.unavailable_reason.is_some());
    assert_eq!(status.last_success_at_ms, Some(43));
    assert!(status.last_refresh_error.is_none());
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
