//! 人工 SQLite 夹具通过正式迁移入口建库，覆盖真实 worker 使用的读写语义。
use super::*;
use assistant_protocol::{
    ModelDiscoveryFormat, ModelFeatureSupport, ModelReasoningMode, ProviderProtocolPreference,
    ProviderType,
};

fn provider(id: &str) -> StoredProvider {
    StoredProvider {
        provider_instance_id: ProviderInstanceId::new(id).unwrap(),
        api_key: SecretValue::new("isolated-secret".into()),
        connection: ProviderConnection {
            display_name: "同名服务商".into(),
            provider_type: ProviderType::Openai,
            endpoint: "http://127.0.0.1:9/v1".into(),
            protocol_preference: ProviderProtocolPreference::ChatCompletions,
            models_path: "/v1/models".into(),
            discovery_format: ModelDiscoveryFormat::OpenAi,
        },
    }
}
fn fixed(id: &str, output: u64) -> ModelFixedConfig {
    ModelFixedConfig {
        origin: assistant_protocol::ModelConfigOrigin::Online,
        selection: ModelSelection {
            provider_instance_id: ProviderInstanceId::new(id).unwrap(),
            model_id: "org/model-same".into(),
        },
        parameters: ModelParameters {
            context_window_tokens: ModelTokenLimit::Known(NonZeroU64::new(8192).unwrap()),
            max_output_tokens: ModelTokenLimit::Known(NonZeroU64::new(output).unwrap()),
            streaming: ModelFeatureSupport::Supported,
            tool_calls: ModelFeatureSupport::Supported,
            reasoning_mode: ModelReasoningMode::Unsupported,
            ..Default::default()
        },
        updated_at_ms: 15,
    }
}
fn engine(home: &std::path::Path) -> StorageEngine {
    StorageEngine::open(home).unwrap()
}
fn count(engine: &StorageEngine, table: &str) -> i64 {
    assert!(["providers", "model_settings", "model_fixed_configs"].contains(&table));
    engine
        .connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn fixed_values_roundtrip_reset_and_provider_delete_preserve_selections_and_other_instances() {
    let home = tempfile::tempdir().unwrap();
    let mut engine = engine(home.path());
    let one = provider("one");
    let two = provider("two");
    engine.put_provider(one.clone()).unwrap();
    engine.put_provider(two.clone()).unwrap();
    let first = fixed("one", 1024);
    let mut second = fixed("two", 2048);
    second.origin = assistant_protocol::ModelConfigOrigin::Manual;
    engine.put_model_fixed_config(first.clone()).unwrap();
    engine.put_model_fixed_config(second.clone()).unwrap();
    let mut changed_origin = second.clone();
    changed_origin.origin = assistant_protocol::ModelConfigOrigin::Online;
    assert!(engine.put_model_fixed_config(changed_origin).is_err());
    let settings = ModelSettings {
        default_model: Some(first.selection.clone()),
        vision_model: Some(second.selection.clone()),
    };
    engine.save_model_settings(settings.clone()).unwrap();
    assert_eq!(
        engine
            .get_model_fixed_config(first.selection.clone())
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        engine
            .get_model_fixed_config(second.selection.clone())
            .unwrap(),
        Some(second.clone())
    );
    assert_eq!(
        engine
            .list_model_fixed_configs(one.provider_instance_id.clone(), 0, 20)
            .unwrap(),
        vec![first.clone()]
    );
    // 模拟用户重置，只删除固定值，重复操作成功；不用网络决定结果。
    engine
        .reset_model_fixed_config(first.selection.clone())
        .unwrap();
    engine
        .reset_model_fixed_config(first.selection.clone())
        .unwrap();
    assert_eq!(engine.load_model_settings().unwrap(), settings);
    assert!(
        engine
            .get_model_fixed_config(first.selection.clone())
            .unwrap()
            .is_none()
    );
    engine.put_model_fixed_config(first.clone()).unwrap();
    let preview = engine
        .provider_usage(one.provider_instance_id.clone())
        .unwrap();
    assert_eq!(preview.fixed_config_count, 1);
    assert!(preview.default_model);
    assert!(!preview.vision_model);
    let mut added_after_preview = first.clone();
    added_after_preview.selection.model_id = "org/added-after-confirmation".into();
    engine.put_model_fixed_config(added_after_preview).unwrap();
    let removed = engine.remove_provider(one.provider_instance_id).unwrap();
    assert_eq!(removed.fixed_config_count, 2);
    assert!(removed.default_model);
    assert!(!removed.vision_model);
    assert_eq!(engine.load_model_settings().unwrap(), settings);
    assert_eq!(engine.load_providers().unwrap(), vec![two]);
    assert_eq!(
        engine
            .get_model_fixed_config(second.selection.clone())
            .unwrap(),
        Some(second.clone())
    );
    assert_eq!(
        (
            count(&engine, "providers"),
            count(&engine, "model_settings"),
            count(&engine, "model_fixed_configs")
        ),
        (1, 1, 1)
    );
    drop(engine);
    let reopened = StorageEngine::open(home.path()).unwrap();
    assert_eq!(reopened.load_model_settings().unwrap(), settings);
    assert_eq!(
        reopened
            .get_model_fixed_config(second.selection.clone())
            .unwrap(),
        Some(second)
    );
    assert_eq!(
        reopened.get_model_fixed_config(first.selection).unwrap(),
        None
    );
}
#[test]
fn provider_update_keeps_fixed_records_and_secrets_are_not_in_debug_output() {
    let home = tempfile::tempdir().unwrap();
    let engine = engine(home.path());
    let mut value = provider("one");
    engine.put_provider(value.clone()).unwrap();
    engine.put_model_fixed_config(fixed("one", 1024)).unwrap();
    value.connection.display_name = "新的显示名".into();
    value.api_key = SecretValue::new("replacement-secret".into());
    engine.put_provider(value.clone()).unwrap();
    assert_eq!(engine.load_providers().unwrap(), vec![value.clone()]);
    assert!(!format!("{value:?}").contains("replacement-secret"));
    assert_eq!(
        (
            count(&engine, "providers"),
            count(&engine, "model_fixed_configs")
        ),
        (1, 1)
    );
}
#[test]
fn invalid_fixed_limits_or_missing_provider_cannot_replace_valid_values() {
    let home = tempfile::tempdir().unwrap();
    let engine = engine(home.path());
    engine.put_provider(provider("one")).unwrap();
    let valid = fixed("one", 1024);
    engine.put_model_fixed_config(valid.clone()).unwrap();
    assert!(engine.put_model_fixed_config(fixed("one", 9000)).is_err());
    let mut inconsistent = valid.clone();
    inconsistent.parameters.reasoning = assistant_protocol::ModelFeatureSupport::Unsupported;
    inconsistent.parameters.reasoning_max_output_tokens =
        ModelTokenLimit::Known(512.try_into().unwrap());
    assert!(engine.put_model_fixed_config(inconsistent).is_err());
    assert_eq!(
        engine
            .get_model_fixed_config(valid.selection.clone())
            .unwrap(),
        Some(valid)
    );
    assert!(
        engine
            .put_model_fixed_config(fixed("missing", 1024))
            .is_err()
    );
    assert_eq!(
        (
            count(&engine, "providers"),
            count(&engine, "model_settings"),
            count(&engine, "model_fixed_configs")
        ),
        (1, 1, 1)
    );
}
