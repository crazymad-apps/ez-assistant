//! 模型管理命令使用独立实例身份，并保留未知参数与显式清空意图。
use super::*;
use crate::{RuntimeCommand, RuntimeCommandResult, SetDefaultModelRequest};

#[test]
fn provider_and_fixed_configuration_commands_round_trip() {
    let provider_instance_id = ProviderInstanceId::new("provider-1").unwrap();
    let selection = ModelSelection {
        provider_instance_id: provider_instance_id.clone(),
        model_id: "vendor/model:latest".into(),
    };
    let connection = ProviderConnection {
        display_name: "同名服务商".into(),
        provider_type: ProviderType::Local,
        endpoint: "http://127.0.0.1:1234/v1".into(),
        protocol_preference: ProviderProtocolPreference::ChatCompletions,
        models_path: "/models".into(),
        discovery_format: ModelDiscoveryFormat::OpenAi,
    };
    let provider = ProviderSummary {
        provider_instance_id: provider_instance_id.clone(),
        connection: connection.clone(),
        has_api_key: true,
    };
    let request = ProviderRequest {
        provider_instance_id: provider_instance_id.clone(),
    };
    let parameters = ModelParameters {
        max_input_tokens: ModelTokenLimit::Invalid,
        ..Default::default()
    };
    let fixed = ModelFixedConfig {
        origin: crate::ModelConfigOrigin::Online,
        selection: selection.clone(),
        parameters: parameters.clone(),
        updated_at_ms: 42,
    };
    let usage = ProviderUsage {
        default_model: true,
        vision_model: true,
        session_count: 27,
        fixed_config_count: 2,
        sessions: vec![ProviderSessionUsage {
            session_id: SessionId::new("archived-1").unwrap(),
            title: "归档会话".into(),
        }],
    };
    let cases = vec![
        (
            "list_providers",
            RuntimeCommand::ListProviders(ListProvidersRequest {}),
            RuntimeCommandResult::ListProviders(vec![provider.clone()]),
        ),
        (
            "create_provider",
            RuntimeCommand::CreateProvider(CreateProviderRequest {
                connection: connection.clone(),
                credential: ProviderCredentialChange::Replace(SecretValue::new(
                    "request-only-secret".into(),
                )),
            }),
            RuntimeCommandResult::CreateProvider(provider.clone()),
        ),
        (
            "update_provider",
            RuntimeCommand::UpdateProvider(UpdateProviderRequest {
                provider_instance_id: provider_instance_id.clone(),
                connection,
                credential: ProviderCredentialChange::Unchanged,
            }),
            RuntimeCommandResult::UpdateProvider(provider),
        ),
        (
            "get_provider_usage",
            RuntimeCommand::GetProviderUsage(request.clone()),
            RuntimeCommandResult::GetProviderUsage(usage.clone()),
        ),
        (
            "delete_provider",
            RuntimeCommand::DeleteProvider(request.clone()),
            RuntimeCommandResult::DeleteProvider(usage),
        ),
        (
            "list_provider_models",
            RuntimeCommand::ListProviderModels(request),
            RuntimeCommandResult::ListProviderModels(vec![DiscoveredModel {
                configuration: None,
                model_id: selection.model_id.clone(),
                display_name: None,
                metadata: parameters.clone(),
            }]),
        ),
        (
            "get_model_settings",
            RuntimeCommand::GetModelSettings(GetModelSettingsRequest {}),
            RuntimeCommandResult::GetModelSettings(ModelSettings {
                default_model: Some(selection.clone()),
                vision_model: None,
            }),
        ),
        (
            "set_default_model",
            RuntimeCommand::SetDefaultModel(SetDefaultModelRequest { selection: None }),
            RuntimeCommandResult::SetDefaultModel(ModelSettings::default()),
        ),
        (
            "get_model_configuration",
            RuntimeCommand::GetModelConfiguration(selection.clone().into()),
            RuntimeCommandResult::GetModelConfiguration(ModelConfigurationDetail {
                field_sources: Default::default(),
                template_document: None,
                template_checked_on: None,
                origin: crate::ModelConfigOrigin::Online,
                selection: selection.clone(),
                parameters: parameters.clone(),
                source: ModelConfigurationSource::Fixed,
                updated_at_ms: Some(42),
            }),
        ),
        (
            "save_model_fixed_config",
            RuntimeCommand::SaveModelFixedConfig(SaveModelFixedConfigRequest {
                origin: crate::ModelConfigOrigin::Online,
                selection: selection.clone(),
                parameters,
            }),
            RuntimeCommandResult::SaveModelFixedConfig(fixed.clone()),
        ),
        (
            "reset_model_fixed_config",
            RuntimeCommand::ResetModelFixedConfig(selection),
            RuntimeCommandResult::ResetModelFixedConfig(()),
        ),
        (
            "list_fixed_model_configs",
            RuntimeCommand::ListFixedModelConfigs(ListFixedModelConfigsRequest {
                provider_instance_id,
                offset: 20,
                limit: 20,
            }),
            RuntimeCommandResult::ListFixedModelConfigs(vec![fixed]),
        ),
    ];
    for (tag, command, result) in cases {
        let wire = serde_json::to_value(&command).unwrap();
        assert_eq!(wire["type"], tag);
        assert_eq!(
            serde_json::from_value::<RuntimeCommand>(wire).unwrap(),
            command
        );
        assert!(!format!("{command:?}").contains("request-only-secret"));
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["type"], tag);
        assert!(!wire.to_string().contains("request-only-secret"));
        assert_eq!(
            serde_json::from_value::<RuntimeCommandResult>(wire).unwrap(),
            result
        );
    }
}

#[test]
fn obsolete_model_management_commands_are_rejected() {
    for tag in [
        "list_models",
        "get_model",
        "create_model",
        "update_model",
        "delete_model",
    ] {
        assert!(
            serde_json::from_value::<RuntimeCommand>(
                serde_json::json!({"type": tag, "payload": {}})
            )
            .is_err()
        );
    }
}
