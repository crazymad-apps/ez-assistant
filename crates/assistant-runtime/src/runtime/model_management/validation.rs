use super::*;
pub(crate) use crate::config::validation::{
    validate_connection, validate_provider_parameters, validate_selection,
};

pub(super) fn credential(
    previous: SecretValue,
    change: ProviderCredentialChange,
) -> RuntimeResult<SecretValue> {
    let value = match change {
        ProviderCredentialChange::Unchanged => previous,
        ProviderCredentialChange::Replace(value) => value,
        ProviderCredentialChange::Clear => SecretValue::new(String::new()),
    };
    if value.expose().len() > 16384 || value.expose().chars().any(char::is_control) {
        return Err(invalid("API Key 过长或含控制字符。"));
    }
    Ok(value)
}
// 套餐凭据不能发到百炼原生列表端点；错误只说明接入类型，不回显凭据。
pub(super) fn validate_provider_credential(
    connection: &ProviderConnection,
    key: &SecretValue,
) -> RuntimeResult<()> {
    if connection.provider_type == assistant_protocol::ProviderType::DashscopeApi
        && key.expose().starts_with("sk-sp-")
    {
        return Err(invalid(
            "套餐密钥应选择百炼套餐类型，不能用于百炼 API 原生接口。",
        ));
    }
    Ok(())
}
