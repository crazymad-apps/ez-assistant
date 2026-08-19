//! 内部脱敏投影到应用协议 DTO 的单向转换。

use assistant_protocol::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
    ModelConfiguration, ModelConfigurationOrigin, ModelKey,
};

use super::domain::{
    ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigState, ModelConfigProjection,
};

pub(crate) fn project_status(
    projection: &ConfigProjection,
    config_path: Option<String>,
    revision: Option<String>,
) -> ConfigurationStatus {
    ConfigurationStatus {
        config_path,
        revision,
        state: state(projection.state),
        schema_version: projection.schema_version,
        default_model: projection.default_model.clone(),
        auxiliary_vision_model: projection.auxiliary_vision_model.clone(),
        issues: projection
            .issues
            .iter()
            .filter(|issue| issue.model_key().is_none())
            .map(issue)
            .collect(),
    }
}

pub(crate) fn project_models(projection: &ConfigProjection) -> Vec<ModelConfiguration> {
    projection.models.iter().map(model).collect()
}

pub(crate) fn project_model_by_key(
    projection: &ConfigProjection,
    model_key: &ModelKey,
) -> Option<ModelConfiguration> {
    projection
        .models
        .iter()
        .find(|model| model.model_key.as_ref() == Some(model_key))
        .map(model)
}

fn model(projection: &ModelConfigProjection) -> ModelConfiguration {
    ModelConfiguration {
        model_key: projection.model_key.clone(),
        display_name: projection.display_name.clone(),
        protocol: projection.protocol.clone(),
        provider: projection.provider.clone(),
        endpoint: projection.endpoint.clone(),
        model: projection.model.clone(),
        context_window_tokens: projection.context_window_tokens,
        max_output_tokens: projection.max_output_tokens,
        agent_max_output_tokens: projection.agent_max_output_tokens,
        effective_max_output_tokens: projection.effective_max_output_tokens,
        supports_image_input: projection.supports_image_input,
        api_key_configured: projection.api_key_configured,
        origin: ModelConfigurationOrigin::ConfigurationFile,
        editable: projection.model_key.is_some(),
        deletable: projection.model_key.is_some(),
        is_default: projection.is_default,
        is_valid: projection.is_valid,
        issues: projection.issues.iter().map(issue).collect(),
    }
}

fn issue(value: &ConfigIssue) -> ConfigurationIssue {
    ConfigurationIssue {
        code: issue_code(value.code()),
        model_key: value.model_key().cloned(),
        message: value.message().to_owned(),
    }
}

fn state(value: ConfigState) -> ConfigurationState {
    match value {
        ConfigState::Missing => ConfigurationState::Missing,
        ConfigState::Invalid => ConfigurationState::Invalid,
        ConfigState::Degraded => ConfigurationState::Degraded,
        ConfigState::Ready => ConfigurationState::Ready,
    }
}

fn issue_code(value: ConfigIssueCode) -> ConfigurationIssueCode {
    match value {
        ConfigIssueCode::InvalidSyntax => ConfigurationIssueCode::InvalidSyntax,
        ConfigIssueCode::UnsupportedSchemaVersion => {
            ConfigurationIssueCode::UnsupportedSchemaVersion
        }
        ConfigIssueCode::InvalidTopLevel => ConfigurationIssueCode::InvalidTopLevel,
        ConfigIssueCode::UnsafeConfigSource => ConfigurationIssueCode::UnsafeConfigSource,
        ConfigIssueCode::ConfigReadFailed => ConfigurationIssueCode::ConfigReadFailed,
        ConfigIssueCode::UnknownField => ConfigurationIssueCode::UnknownField,
        ConfigIssueCode::MissingField => ConfigurationIssueCode::MissingField,
        ConfigIssueCode::InvalidModelKey => ConfigurationIssueCode::InvalidModelKey,
        ConfigIssueCode::InvalidModel => ConfigurationIssueCode::InvalidModel,
        ConfigIssueCode::UnsupportedProtocol => ConfigurationIssueCode::UnsupportedProtocol,
        ConfigIssueCode::InvalidProvider => ConfigurationIssueCode::InvalidProvider,
        ConfigIssueCode::InvalidEndpoint => ConfigurationIssueCode::InvalidEndpoint,
        ConfigIssueCode::MissingCredential => ConfigurationIssueCode::MissingCredential,
        ConfigIssueCode::InvalidLimit => ConfigurationIssueCode::InvalidLimit,
        ConfigIssueCode::InvalidPolicy => ConfigurationIssueCode::InvalidPolicy,
        ConfigIssueCode::UnsupportedProfileCombination => {
            ConfigurationIssueCode::UnsupportedProfileCombination
        }
        ConfigIssueCode::DefaultModelUnavailable => ConfigurationIssueCode::DefaultModelUnavailable,
    }
}
