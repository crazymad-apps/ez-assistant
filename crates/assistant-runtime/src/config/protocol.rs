//! 内部脱敏投影到应用协议 DTO 的单向转换。

use assistant_protocol::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
};

use super::domain::{ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigState};

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
        issues: projection.issues.iter().map(issue).collect(),
    }
}

fn issue(value: &ConfigIssue) -> ConfigurationIssue {
    ConfigurationIssue {
        code: issue_code(value.code()),
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
        ConfigIssueCode::InvalidLimit => ConfigurationIssueCode::InvalidLimit,
        ConfigIssueCode::InvalidPolicy => ConfigurationIssueCode::InvalidPolicy,
    }
}
