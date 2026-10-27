//! Sub2API 的 Codex 目录投影；提示词、工具运行方式和本地压缩阈值均不进入模型配置。
use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub(super) fn page(document: &Value) -> Result<ParsedPage, ModelDiscoveryError> {
    let entries = document["models"].as_array().ok_or_else(invalid)?;
    check_count(entries.len())?;
    let models = entries.iter().map(model).collect::<Result<_, _>>()?;
    Ok(ParsedPage {
        models,
        total: None,
    })
}

fn model(raw: &Value) -> Result<DiscoveredModel, ModelDiscoveryError> {
    let mut model = identity(raw, "slug")?;
    if let Some(name) = raw.get("display_name").filter(|value| !value.is_null()) {
        model.display_name = Some(
            name.as_str()
                .filter(|name| valid_text(name, 256))
                .ok_or_else(invalid)?
                .to_owned(),
        );
    }
    let p = &mut model.metadata;
    let context = raw
        .get("context_window")
        .filter(|value| !value.is_null())
        .unwrap_or(&raw["max_context_window"]);
    p.context_window_tokens = token_limit(context);
    if let Some(modalities) = raw.get("input_modalities").filter(|value| !value.is_null()) {
        let modalities = modalities.as_array().ok_or_else(invalid)?;
        if modalities.iter().any(|value| !value.is_string()) {
            return Err(invalid());
        }
        p.image_input = if modalities.iter().any(|value| value == "image") {
            ModelFeatureSupport::Supported
        } else {
            ModelFeatureSupport::Unsupported
        };
    }
    match boolean_feature(&raw["supports_parallel_tool_calls"])? {
        ModelFeatureSupport::Supported => p.tool_calls = ModelFeatureSupport::Supported,
        // 不支持并行不能解释为不支持串行工具调用。
        ModelFeatureSupport::Unsupported | ModelFeatureSupport::Unknown => {}
    }
    if let Some(levels) = raw
        .get("supported_reasoning_levels")
        .filter(|value| !value.is_null())
    {
        let levels = levels
            .as_array()
            .filter(|values| values.len() <= 6)
            .ok_or_else(invalid)?;
        let mut seen = BTreeSet::new();
        let mut mapping = BTreeMap::new();
        for level in levels {
            let effort = level["effort"].as_str().ok_or_else(invalid)?;
            if !seen.insert(effort) {
                return Err(invalid());
            }
            if effort != "none" {
                let key: ReasoningEffortKey =
                    serde_json::from_value(Value::String(effort.into())).map_err(|_| invalid())?;
                mapping.insert(key, effort.to_owned());
            }
        }
        let selected = raw
            .get("default_reasoning_level")
            .filter(|value| !value.is_null());
        let selected = selected
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| seen.contains(value))
                    .ok_or_else(invalid)
            })
            .transpose()?;
        if mapping.is_empty() {
            p.reasoning_efforts = Some(mapping);
        } else {
            p.reasoning = ModelFeatureSupport::Supported;
            // none 只表示模型默认行为，不制造“关闭思考”的能力声明。
            if let Some(selected) = selected.filter(|value| *value != "none") {
                p.default_reasoning_effort = Some(
                    serde_json::from_value(Value::String(selected.into()))
                        .map_err(|_| invalid())?,
                );
                p.reasoning_efforts = Some(mapping);
            }
        }
    } else if raw
        .get("default_reasoning_level")
        .is_some_and(|value| !value.is_null())
    {
        return Err(invalid());
    }
    Ok(model)
}
