//! 只映射已经核对的接口字段；未知能力和非法限制必须保持可观察，不用静态目录补齐。

use std::num::NonZeroU64;

use assistant_runtime::{
    DiscoveredModel, ModelDiscoveryError, ModelFeatureSupport, ModelParameters, ModelReasoningMode,
    ModelTokenLimit, ReasoningEffortKey,
};
use serde_json::Value;

use super::{DiscoveryFormat, ErrorKind, MAX_MODELS, PAGE_SIZE};

/// 本次已校验的分页数据；所有分页完成前不会向调用方发布条目。
pub(super) struct ParsedPage {
    pub(super) models: Vec<DiscoveredModel>,
    pub(super) total: Option<usize>,
}

pub(super) fn page(
    bytes: &[u8],
    format: DiscoveryFormat,
    expected_page: u64,
) -> Result<ParsedPage, ModelDiscoveryError> {
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|source| ModelDiscoveryError::with_source(ErrorKind::InvalidResponse, source))?;
    match format {
        DiscoveryFormat::DashScope => dashscope_page(&document, expected_page),
        DiscoveryFormat::OpenAi | DiscoveryFormat::Vllm | DiscoveryFormat::Moonshot => {
            openai_page(&document, format)
        }
    }
}

fn openai_page(
    document: &Value,
    format: DiscoveryFormat,
) -> Result<ParsedPage, ModelDiscoveryError> {
    if document.get("object").and_then(Value::as_str) != Some("list")
        || document.get("has_more").is_some_and(|more| more != false)
    {
        return Err(invalid());
    }
    let entries = document
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    check_count(entries.len())?;
    let models = entries
        .iter()
        .map(|entry| {
            let mut model = identity(entry, "id")?;
            if entry.get("object").and_then(Value::as_str) != Some("model") {
                return Err(invalid());
            }
            // max_model_len 是 vLLM 的显式扩展；普通兼容服务不能仅凭同名字段被猜成 vLLM。
            if matches!(format, DiscoveryFormat::Vllm) {
                model.metadata.context_window_tokens = token_limit(&entry["max_model_len"]);
            }
            if matches!(format, DiscoveryFormat::Moonshot) {
                model.metadata = moonshot_metadata(entry)?;
                if let Some(name) = entry.get("display_name").filter(|value| !value.is_null()) {
                    model.display_name = Some(
                        name.as_str()
                            .filter(|name| valid_text(name, 256))
                            .ok_or_else(invalid)?
                            .to_owned(),
                    );
                }
            }
            Ok(model)
        })
        .collect::<Result<_, _>>()?;
    Ok(ParsedPage {
        models,
        total: None,
    })
}

fn dashscope_page(document: &Value, expected_page: u64) -> Result<ParsedPage, ModelDiscoveryError> {
    match document.get("success").and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => return Err(ModelDiscoveryError::new(ErrorKind::Unavailable)),
        None => return Err(invalid()),
    }
    let output = &document["output"];
    let total = output["total"].as_u64().ok_or_else(invalid)?;
    let total = usize::try_from(total).map_err(|_| invalid())?;
    check_count(total)?;
    let page_size = output["page_size"].as_u64().ok_or_else(invalid)?;
    if output["page_no"].as_u64() != Some(expected_page) || !(1..=PAGE_SIZE).contains(&page_size) {
        return Err(invalid());
    }
    let entries = output["models"].as_array().ok_or_else(invalid)?;
    if entries.len() as u64 > page_size || (entries.is_empty() && total != 0) {
        return Err(invalid());
    }
    let models = entries
        .iter()
        .map(dashscope_model)
        .collect::<Result<_, _>>()?;
    Ok(ParsedPage {
        models,
        total: Some(total),
    })
}

fn dashscope_model(entry: &Value) -> Result<DiscoveredModel, ModelDiscoveryError> {
    let mut model = identity(entry, "model")?;
    if let Some(name) = entry.get("name").filter(|name| !name.is_null()) {
        let name = name
            .as_str()
            .filter(|name| valid_text(name, 256))
            .ok_or_else(invalid)?;
        model.display_name = Some(name.to_owned());
    }
    let info = &entry["model_info"];
    if !info.is_null() && !info.is_object() {
        return Err(invalid());
    }
    let inference = &entry["inference_metadata"];
    if !inference.is_null() && !inference.is_object() {
        return Err(invalid());
    }
    let mut metadata = ModelParameters {
        context_window_tokens: token_limit(&info["context_window"]),
        max_input_tokens: token_limit(&info["max_input_tokens"]),
        max_output_tokens: token_limit(&info["max_output_tokens"]),
        reasoning_max_input_tokens: token_limit(&info["reasoning_max_input_tokens"]),
        reasoning_max_output_tokens: token_limit(&info["reasoning_max_output_tokens"]),
        image_input: feature(&inference["request_modality"], "Image")?,
        tool_calls: feature(&entry["features"], "function-calling")?,
        reasoning: feature(&entry["capabilities"], "Reasoning")?,
        ..ModelParameters::default()
    };
    // 总窗口是输入／输出共同约束；矛盾字段标为非法，不以裁剪数值掩盖接口问题。
    for limit in [
        &mut metadata.max_input_tokens,
        &mut metadata.max_output_tokens,
        &mut metadata.reasoning_max_input_tokens,
        &mut metadata.reasoning_max_output_tokens,
    ] {
        if let (ModelTokenLimit::Known(context), ModelTokenLimit::Known(value)) =
            (metadata.context_window_tokens, *limit)
            && value > context
        {
            *limit = ModelTokenLimit::Invalid;
        }
    }
    model.metadata = metadata;
    Ok(model)
}

fn identity(entry: &Value, key: &str) -> Result<DiscoveredModel, ModelDiscoveryError> {
    let id = entry
        .get(key)
        .and_then(Value::as_str)
        .filter(|id| valid_text(id, 512))
        .ok_or_else(invalid)?;
    Ok(DiscoveredModel {
        configuration: None,
        model_id: id.to_owned(),
        display_name: None,
        metadata: ModelParameters::default(),
    })
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= max_bytes
        && !value.chars().any(char::is_control)
}

fn token_limit(value: &Value) -> ModelTokenLimit {
    if value.is_null() {
        return ModelTokenLimit::Unknown;
    }
    // 后续需要投影到 TypeScript；不能把超过安全整数范围的限制悄悄舍入。
    match value
        .as_u64()
        .filter(|value| *value <= 9_007_199_254_740_991)
        .and_then(NonZeroU64::new)
    {
        Some(tokens) => ModelTokenLimit::Known(tokens),
        None => ModelTokenLimit::Invalid,
    }
}

fn feature(value: &Value, name: &str) -> Result<ModelFeatureSupport, ModelDiscoveryError> {
    if value.is_null() {
        return Ok(ModelFeatureSupport::Unknown);
    }
    let values = value.as_array().ok_or_else(invalid)?;
    if values.iter().any(|value| !value.is_string()) {
        return Err(invalid());
    }
    Ok(if values.iter().any(|value| value.as_str() == Some(name)) {
        ModelFeatureSupport::Supported
    } else {
        ModelFeatureSupport::Unsupported
    })
}

fn check_count(count: usize) -> Result<(), ModelDiscoveryError> {
    if count > MAX_MODELS {
        return Err(ModelDiscoveryError::new(ErrorKind::ResponseTooLarge));
    }
    Ok(())
}

fn invalid() -> ModelDiscoveryError {
    ModelDiscoveryError::new(ErrorKind::InvalidResponse)
}

/// Kimi Coding 的实测字段；不将 CLI 示例默认强度或开放 API 输出预算当成接口返回。
fn moonshot_metadata(entry: &Value) -> Result<ModelParameters, ModelDiscoveryError> {
    let reasoning = boolean_feature(&entry["supports_reasoning"])?;
    let reasoning_mode = match entry
        .get("supports_thinking_type")
        .filter(|value| !value.is_null())
    {
        None => ModelReasoningMode::Unknown,
        Some(value) if value.as_str() == Some("only") => ModelReasoningMode::Always,
        // 未取得其他字符串的语义证据时失败，不能将其猜成可关闭思考。
        Some(_) => return Err(invalid()),
    };
    if reasoning_mode == ModelReasoningMode::Always && reasoning == ModelFeatureSupport::Unsupported
    {
        return Err(invalid());
    }
    let mut result = ModelParameters {
        context_window_tokens: token_limit(&entry["context_length"]),
        image_input: boolean_feature(&entry["supports_image_in"])?,
        // dynamic_tools 表示动态工具发现，不等同于普通 Function Calling。
        // K2.7 返回 false 仍支持静态工具；普通工具能力交由显式字段／规格模板补充。
        tool_calls: ModelFeatureSupport::Unknown,
        reasoning,
        reasoning_mode,
        ..ModelParameters::default()
    };
    let efforts = &entry["think_efforts"];
    if efforts.is_null() {
        return Ok(result);
    }
    if !efforts.is_object() {
        return Err(invalid());
    }
    match efforts["support"].as_bool() {
        Some(true) => {
            let values = efforts["valid_efforts"].as_array().ok_or_else(invalid)?;
            let parsed = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .and_then(ReasoningEffortKey::parse)
                        .ok_or_else(invalid)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if parsed.is_empty()
                || parsed.len() > 5
                || parsed
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != parsed.len()
            {
                return Err(invalid());
            }
            let default = efforts["default_effort"]
                .as_str()
                .and_then(ReasoningEffortKey::parse)
                .ok_or_else(invalid)?;
            if !parsed.contains(&default) || reasoning == ModelFeatureSupport::Unsupported {
                return Err(invalid());
            }
            result.reasoning_efforts = Some(
                parsed
                    .into_iter()
                    .zip(values.iter())
                    .map(|(key, value)| {
                        (
                            key,
                            value.as_str().expect("validated effort string").to_owned(),
                        )
                    })
                    .collect(),
            );
            result.default_reasoning_effort = Some(default);
        }
        Some(false) => {
            if efforts.get("valid_efforts").is_some_and(|value| {
                !value.is_null() && value.as_array().is_none_or(|values| !values.is_empty())
            }) || efforts
                .get("default_effort")
                .is_some_and(|value| !value.is_null())
            {
                return Err(invalid());
            }
            result.reasoning_efforts = Some(Default::default());
        }
        None => return Err(invalid()),
    }
    Ok(result)
}

fn boolean_feature(value: &Value) -> Result<ModelFeatureSupport, ModelDiscoveryError> {
    match value {
        Value::Null => Ok(ModelFeatureSupport::Unknown),
        Value::Bool(true) => Ok(ModelFeatureSupport::Supported),
        Value::Bool(false) => Ok(ModelFeatureSupport::Unsupported),
        _ => Err(invalid()),
    }
}
