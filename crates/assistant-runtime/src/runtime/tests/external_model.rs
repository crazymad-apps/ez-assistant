//! 外部模型状态只在 Runtime 内存生效；旧选择、旧 Arc 和本地 Store 均不能成为旁路。

use super::*;
use crate::{ExternalModelConfiguration, ExternalModelSet, ModelSource};

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
    fixture_with_store(Arc::new(crate::storage::VolatileRuntimeStore::default())).await
}

async fn fixture_with_store(store: Arc<dyn RuntimeStore>) -> AssistantRuntime {
    let runtime = AssistantRuntime::open(
        RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
        Arc::new(MissingConfigSource),
        Arc::new(ExternalFactory),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolRegistry::new().snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store,
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .unwrap();
    // 个人配置仍从原 Store 恢复；组织发布只改变组织部分。
    model_fixture::seed(&runtime, "local-secret-must-not-be-used").await;
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
}

fn configuration(model: &str) -> Arc<ExternalModelConfiguration> {
    Arc::new(
        ExternalModelConfiguration::new(
            test_model_selection("fixture").provider_instance_id,
            model_fixture::provider("").connection,
            model.to_owned(),
            model_fixture::parameters(),
            42,
            None,
        )
        .unwrap(),
    )
}

async fn publish(runtime: &AssistantRuntime, value: Arc<ExternalModelConfiguration>) {
    let mut gate = runtime.external_model_publication().await.unwrap();
    gate.publish(model_set(value), 42).unwrap();
    gate.finish();
}

fn model_set(value: Arc<ExternalModelConfiguration>) -> ExternalModelSet {
    let selection = value.selection().clone();
    ExternalModelSet {
        default_model: Some(selection.clone()),
        allow_member_selection: false,
        models: [(selection, Ok(value))].into(),
    }
}

#[tokio::test]
async fn application_read_keeps_its_frozen_model_inputs_during_deferred_external_notification() {
    let store = Arc::new(super::store::FaultInjectingStore::healthy());
    let runtime = fixture_with_store(store.clone()).await;
    publish(&runtime, configuration("old-model")).await;
    let before = runtime.event_sender.sequence();
    store.block_next_summary_read();
    let read_finished = tokio::sync::Notify::new();
    let read = async {
        let result = runtime.get_application_snapshot(Default::default()).await;
        read_finished.notify_one();
        result.unwrap().snapshot
    };
    let update = async {
        store.wait_for_summary_read().await;
        let mut connection = model_fixture::provider("").connection;
        connection.display_name = "New managed provider".to_owned();
        let next = Arc::new(
            ExternalModelConfiguration::new(
                test_model_selection("fixture").provider_instance_id,
                connection,
                "new-model".to_owned(),
                model_fixture::parameters(),
                43,
                None,
            )
            .unwrap(),
        );
        let mut publication = runtime.external_model_publication().await.unwrap();
        publication.publish(model_set(next), 43).unwrap();
        // 模拟 Host 在身份锁内提交后尚未 finish；快照不能持读门等通知而互锁，
        // 也不能把已经复制的旧 Provider 与晚到的新默认选择重新拼接。
        assert_eq!(runtime.event_sender.sequence(), before);
        store.release_summary_read();
        read_finished.notified().await;
        publication.finish();
    };
    let (snapshot, ()) =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(read, update) })
            .await
            .expect("snapshot and deferred publication do not wait on one another");
    assert_eq!(snapshot.observed_sequence, before);
    assert_ne!(
        snapshot.value.providers[0].connection.display_name,
        "New managed provider"
    );
    assert_eq!(
        snapshot
            .value
            .model_settings
            .default_model
            .unwrap()
            .model_id,
        "old-model"
    );
    let next = runtime
        .get_application_snapshot(Default::default())
        .await
        .unwrap()
        .snapshot;
    assert!(next.observed_sequence > before);
    assert_eq!(
        next.value
            .providers
            .iter()
            .find(|provider| provider.source
                == assistant_protocol::ModelSelectionSource::Organization)
            .unwrap()
            .connection
            .display_name,
        "New managed provider"
    );
    assert_eq!(
        next.value.model_settings.default_model.unwrap().model_id,
        "new-model"
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn organization_and_local_selections_keep_distinct_credentials_even_with_identical_ids() {
    let runtime = fixture().await;
    let snapshot = runtime.config_registry.snapshot().unwrap();
    let personal = test_model_selection("fixture");
    assert!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .is_none()
    );
    publish(&runtime, configuration("fixture")).await;
    let providers = runtime.list_providers().unwrap();
    assert_eq!(providers.len(), 2);
    assert_eq!(
        providers[0].provider_instance_id,
        providers[1].provider_instance_id
    );
    let organization = runtime.get_model_settings().unwrap().default_model.unwrap();
    let catalog = runtime
        .list_organization_provider_models(organization.provider_instance_id.clone())
        .unwrap();
    assert_eq!(catalog.models[0].model_id, organization.model_id);
    let local = runtime
        .config_registry
        .prepare_model(&snapshot, Some(&personal), runtime.store.as_ref())
        .await
        .unwrap();
    assert_eq!(local.selection, personal);
    assert_eq!(local.model.api_key(), "local-secret-must-not-be-used");
    assert!(local.model.external_configuration.is_none());
    let remote = runtime
        .config_registry
        .configured_model(&snapshot, None, runtime.store.as_ref())
        .await
        .unwrap();
    assert_eq!(remote.selection, organization);
    assert!(remote.model.api_key().is_empty());
    assert!(remote.model.external_configuration.is_some());
    assert_eq!(
        runtime
            .store
            .load_model_settings()
            .await
            .unwrap()
            .default_model,
        Some(personal.clone())
    );
    publish(&runtime, configuration("new-default")).await;
    assert!(local.ensure_current(&runtime.config_registry).is_ok());
    assert!(remote.ensure_current(&runtime.config_registry).is_err());
    assert!(
        runtime
            .config_registry
            .prepare_model(&snapshot, Some(&organization), runtime.store.as_ref())
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .config_registry
            .prepare_model(&snapshot, Some(&personal), runtime.store.as_ref())
            .await
            .unwrap()
            .selection,
        personal
    );
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
    let settings = runtime.get_model_settings().unwrap();
    assert_eq!(settings.default_model.as_ref(), Some(current.selection()));
    assert_eq!(settings.management.unwrap().unavailable_models.len(), 1);
    assert!(matches!(
        runtime
            .config_registry
            .configured_model(&snapshot, None, runtime.store.as_ref())
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    publish(&runtime, configuration("new-model")).await;
    let mut gate = runtime.external_model_publication().await.unwrap();
    gate.publish(
        ExternalModelSet {
            default_model: None,
            allow_member_selection: false,
            models: Default::default(),
        },
        43,
    )
    .unwrap();
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
