//! 本地与外部模型共用的纯输入校验，不访问配置状态、存储或网络。

use crate::{RuntimeError, RuntimeResult};
use assistant_protocol::{ModelParameters, ModelSelection, ProviderConnection};

pub(crate) fn validate_selection(selection: &ModelSelection) -> RuntimeResult<()> {
    validate_model_id(&selection.model_id)
}

fn validate_model_id(model_id: &str) -> RuntimeResult<()> {
    if model_id.trim().is_empty() || model_id.len() > 1024 || model_id.chars().any(char::is_control)
    {
        return Err(invalid("模型 ID 为空、过长或含控制字符。"));
    }
    Ok(())
}

pub(crate) fn validate_connection(connection: &ProviderConnection) -> RuntimeResult<()> {
    if connection.display_name.trim().is_empty()
        || connection.display_name.len() > 256
        || connection.display_name.chars().any(char::is_control)
    {
        return Err(invalid("请填写有效的服务商名称。"));
    }
    if connection.endpoint.len() > 8192 {
        return Err(invalid("服务地址过长。"));
    }
    let url = url::Url::parse(&connection.endpoint)
        .map_err(|_| invalid("服务地址必须是有效的 HTTP 或 HTTPS 地址。"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid("服务地址不允许 URL 凭据、查询参数或片段。"));
    }
    let path = &connection.models_path;
    if !path.is_empty()
        && (!path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['?', '#', '%', '\\'])
            || path.split('/').any(|part| matches!(part, "." | ".."))
            || path.chars().any(char::is_control)
            || path.len() > 2048)
    {
        return Err(invalid(
            "模型列表路径必须是同源绝对路径，不含转义或查询参数。",
        ));
    }
    if connection.discovery_format != connection.provider_type.discovery_format() {
        return Err(invalid(
            "模型列表格式与服务商类型不匹配，请重新选择服务商类型。",
        ));
    }
    crate::config::resolve_provider_protocol(connection)?;
    Ok(())
}
pub(crate) fn validate_provider_parameters(
    connection: &ProviderConnection,
    parameters: &ModelParameters,
) -> RuntimeResult<()> {
    validate_connection(connection)?;
    use assistant_protocol::ModelToolImageProjection::*;
    if parameters.tool_image_projection == NativeToolResult
        && crate::config::resolve_provider_protocol(connection)?
            == crate::ModelProtocol::OpenAiChatCompletions
    {
        return Err(invalid(
            "Chat Completions 不支持工具结果原生图片，请调整固定配置。",
        ));
    }
    Ok(())
}

fn invalid(reason: &'static str) -> RuntimeError {
    RuntimeError::InvalidRequest { reason }
}
