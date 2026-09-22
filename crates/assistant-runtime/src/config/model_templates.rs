//! 配置详情与执行准备共用的厂商规格补齐；只填在线未知字段，不扩充目录或覆盖固定记录。
use assistant_protocol::{
    ModelConfigurationDetail, ModelConfigurationSource, ModelFeatureSupport, ModelParameters,
    ModelReasoningMode, ModelSelection, ModelTokenLimit, ModelToolImageProjection,
    ProviderConnection, ProviderType,
};
use std::collections::BTreeMap;

pub(crate) fn configuration_detail(
    connection: &ProviderConnection,
    selection: ModelSelection,
    mut parameters: ModelParameters,
) -> ModelConfigurationDetail {
    let (template, document, checked_on) =
        specification(connection.provider_type, &selection.model_id);
    let mut template = template;
    if let Ok(protocol) = super::managed_model::resolve_provider_protocol(connection)
        && let Some(entry) = templates().iter().find(|entry| {
            entry.provider_type == connection.provider_type && entry.model_id == selection.model_id
        })
        && let Some(overrides) = entry.protocol_parameters.get(match protocol {
            super::ModelProtocol::OpenAiResponses => "open_ai_responses",
            super::ModelProtocol::OpenAiChatCompletions => "open_ai_chat_completions",
        })
    {
        template = overrides.clone();
    }
    let mut sources = BTreeMap::new();
    // 只补 Unknown，Invalid 和明确不支持均是在线事实，不能被模板掩盖。
    macro_rules! fill {
        ($field:ident, $unknown:pat) => {{
            let source = if matches!(parameters.$field, $unknown) {
                if matches!(template.$field, $unknown) {
                    ModelConfigurationSource::Unconfigured
                } else {
                    parameters.$field = std::mem::take(&mut template.$field);
                    ModelConfigurationSource::Template
                }
            } else {
                ModelConfigurationSource::Online
            };
            sources.insert(stringify!($field).to_owned(), source);
        }};
    }
    fill!(context_window_tokens, ModelTokenLimit::Unknown);
    fill!(max_input_tokens, ModelTokenLimit::Unknown);
    fill!(max_output_tokens, ModelTokenLimit::Unknown);
    fill!(reasoning_max_input_tokens, ModelTokenLimit::Unknown);
    fill!(reasoning_max_output_tokens, ModelTokenLimit::Unknown);
    fill!(streaming, ModelFeatureSupport::Unknown);
    fill!(image_input, ModelFeatureSupport::Unknown);
    fill!(tool_calls, ModelFeatureSupport::Unknown);
    fill!(tool_image_projection, ModelToolImageProjection::Unknown);
    for (name, value, fallback) in [
        (
            "tool_choice.auto",
            &mut parameters.tool_choice.auto,
            template.tool_choice.auto,
        ),
        (
            "tool_choice.none",
            &mut parameters.tool_choice.none,
            template.tool_choice.none,
        ),
    ] {
        let source = if *value != ModelFeatureSupport::Unknown {
            ModelConfigurationSource::Online
        } else if fallback != ModelFeatureSupport::Unknown {
            *value = fallback;
            ModelConfigurationSource::Template
        } else {
            ModelConfigurationSource::Unconfigured
        };
        sources.insert(name.into(), source);
    }
    // 在线已否定思考时，不能用模板强度制造矛盾配置。
    if parameters.reasoning != ModelFeatureSupport::Unsupported
        && parameters.reasoning_mode != ModelReasoningMode::Unsupported
    {
        fill!(reasoning, ModelFeatureSupport::Unknown);
        fill!(reasoning_mode, ModelReasoningMode::Unknown);
        let effort_source = if parameters.reasoning_efforts.is_some() {
            ModelConfigurationSource::Online
        } else if template.reasoning_efforts.is_some() {
            parameters.reasoning_efforts = template.reasoning_efforts.take();
            ModelConfigurationSource::Template
        } else {
            ModelConfigurationSource::Unconfigured
        };
        sources.insert("reasoning_efforts".into(), effort_source);
        if parameters.default_reasoning_effort.is_none()
            && template.default_reasoning_effort.is_some_and(|key| {
                parameters
                    .reasoning_efforts
                    .as_ref()
                    .is_some_and(|values| values.contains_key(&key))
            })
        {
            parameters.default_reasoning_effort = template.default_reasoning_effort;
            sources.insert(
                "default_reasoning_effort".into(),
                ModelConfigurationSource::Template,
            );
        }
    }
    ModelConfigurationDetail {
        origin: assistant_protocol::ModelConfigOrigin::Online,
        selection,
        parameters,
        source: ModelConfigurationSource::Online,
        updated_at_ms: None,
        field_sources: sources,
        template_document: document.map(str::to_owned),
        template_checked_on: checked_on.map(str::to_owned),
    }
}

/// 内嵌资产只解析一次；无效资产不猜测能力，由构建验证拒绝交付。
fn templates() -> &'static [Template] {
    static TEMPLATES: std::sync::OnceLock<Vec<Template>> = std::sync::OnceLock::new();
    TEMPLATES.get_or_init(|| {
        parse_templates(include_str!(
            "../../../../packages/assistant-protocol/resources/model-templates.json"
        ))
        .unwrap_or_default()
    })
}

struct Template {
    provider_type: ProviderType,
    model_id: String,
    parameters: ModelParameters,
    protocol_parameters: BTreeMap<String, ModelParameters>,
    document: String,
    checked_on: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateInput {
    provider_type: ProviderType,
    model_id: String,
    parameters: serde_json::Value,
    #[serde(default)]
    protocol_parameters: BTreeMap<String, serde_json::Value>,
    document: String,
    checked_on: String,
}

fn parse_templates(text: &str) -> Result<Vec<Template>, String> {
    let entries: Vec<TemplateInput> =
        serde_json::from_str(text).map_err(|_| "invalid template JSON")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut templates = Vec::new();
    for entry in entries {
        if !seen.insert((format!("{:?}", entry.provider_type), entry.model_id.clone())) {
            return Err("duplicate template identity".into());
        }
        let parameters = patch_parameters(&ModelParameters::default(), entry.parameters)?;
        let mut protocols = BTreeMap::new();
        for (protocol, patch) in entry.protocol_parameters {
            if !matches!(
                protocol.as_str(),
                "open_ai_responses" | "open_ai_chat_completions"
            ) {
                return Err("unknown template protocol".into());
            }
            protocols.insert(protocol, patch_parameters(&parameters, patch)?);
        }
        templates.push(Template {
            provider_type: entry.provider_type,
            model_id: entry.model_id,
            parameters,
            protocol_parameters: protocols,
            document: entry.document,
            checked_on: entry.checked_on,
        });
    }
    Ok(templates)
}

/// JSON 只覆盖显式字段；tool_choice 为字段组，档位映射则整份替换，空集合不能丢失。
fn patch_parameters(
    base: &ModelParameters,
    patch: serde_json::Value,
) -> Result<ModelParameters, String> {
    let mut value = serde_json::to_value(base).map_err(|_| "invalid template parameters")?;
    let patch = patch
        .as_object()
        .ok_or("template parameters must be object")?;
    for (key, field) in patch {
        if key == "tool_choice" {
            let choices = field.as_object().ok_or("tool_choice must be object")?;
            for (name, choice) in choices {
                value[key][name] = choice.clone();
            }
        } else {
            value[key] = field.clone();
        }
    }
    serde_json::from_value(value).map_err(|_| "invalid template parameter field".into())
}

fn specification(
    provider: ProviderType,
    model: &str,
) -> (ModelParameters, Option<&'static str>, Option<&'static str>) {
    match templates()
        .iter()
        .find(|entry| entry.provider_type == provider && entry.model_id == model)
    {
        Some(entry) => (
            entry.parameters.clone(),
            Some(entry.document.as_str()),
            Some(entry.checked_on.as_str()),
        ),
        None => (ModelParameters::default(), None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_protocol::ReasoningEffortKey;
    fn known(value: u64) -> ModelTokenLimit {
        ModelTokenLimit::Known(std::num::NonZeroU64::new(value).unwrap())
    }

    #[test]
    fn shared_parameter_vectors_match_protocol_overrides_and_unknown_only_merging() {
        let vectors: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../../packages/assistant-protocol/fixtures/model-parameters.json"
        ))
        .unwrap();
        for vector in vectors {
            let connection = ProviderConnection {
                provider_type: ProviderType::Openai,
                protocol_preference: serde_json::from_value(vector["protocol_preference"].clone()).unwrap(),
                ..serde_json::from_value(serde_json::json!({"display_name":"fixture", "provider_type":"openai", "endpoint":"https://example.test/v1", "protocol_preference":"auto", "models_path":"", "discovery_format":"openai"})).unwrap()
            };
            let selection = ModelSelection {
                provider_instance_id: assistant_protocol::ProviderInstanceId::new("fixture")
                    .unwrap(),
                model_id: "gpt-6-astra".into(),
            };
            let detail = configuration_detail(
                &connection,
                selection,
                patch_parameters(&ModelParameters::default(), vector["online"].clone()).unwrap(),
            );
            let parameters = serde_json::to_value(detail.parameters).unwrap();
            let sources = serde_json::to_value(detail.field_sources).unwrap();
            for (key, expected) in vector["expected"].as_object().unwrap() {
                assert_eq!(&parameters[key], expected, "{}: {key}", vector["name"]);
            }
            for (key, expected) in vector["sources"].as_object().unwrap() {
                assert_eq!(&sources[key], expected, "{}: {key}", vector["name"]);
            }
        }
    }

    #[test]
    fn embedded_asset_is_valid_for_every_model_and_protocol() {
        let entries = parse_templates(include_str!(
            "../../../../packages/assistant-protocol/resources/model-templates.json"
        ))
        .expect("checked-in template asset must be valid");
        assert!(!entries.is_empty());
        for entry in entries {
            // 模板允许保留未知必需值，不把它伪装为可直接执行的完整规格。
            assert!(matches!(
                crate::validate_fixed_model_parameters(&entry.parameters),
                Ok(_) | Err(crate::ModelParameterError::MissingRequired(_))
            ));
            for (protocol, parameters) in entry.protocol_parameters {
                assert!(matches!(
                    crate::validate_fixed_model_parameters(&parameters),
                    Ok(_) | Err(crate::ModelParameterError::MissingRequired(_))
                ));
                if protocol == "open_ai_chat_completions" {
                    assert_ne!(
                        parameters.tool_image_projection,
                        ModelToolImageProjection::NativeToolResult
                    );
                }
            }
        }
    }

    #[test]
    fn partial_overrides_replace_efforts_but_merge_tool_choice_fields() {
        let base = specification(ProviderType::Openai, "gpt-6-astra").0;
        let value = patch_parameters(&base, serde_json::json!({"reasoning_efforts": {}, "default_reasoning_effort": null, "tool_choice": {"auto": "unsupported"}})).unwrap();
        assert!(value.reasoning_efforts.unwrap().is_empty());
        assert_eq!(value.tool_choice.auto, ModelFeatureSupport::Unsupported);
        assert_eq!(value.tool_choice.none, base.tool_choice.none);
        assert!(patch_parameters(&base, serde_json::json!({"new_field": true})).is_err());
    }

    #[test]
    fn deepseek_flash_and_legacy_aliases_share_the_v4_1_multimodal_template() {
        for model in [
            "deepseek-flash",
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
        ] {
            let (parameters, document, checked_on) = specification(ProviderType::Deepseek, model);
            assert_eq!(parameters.context_window_tokens, known(1_000_000));
            assert_eq!(parameters.max_output_tokens, known(384_000));
            assert_eq!(parameters.streaming, ModelFeatureSupport::Supported);
            assert_eq!(parameters.image_input, ModelFeatureSupport::Supported);
            assert_eq!(parameters.tool_calls, ModelFeatureSupport::Supported);
            assert_eq!(parameters.tool_choice.auto, ModelFeatureSupport::Supported);
            assert_eq!(
                document,
                Some("https://api-docs.deepseek.com/quick_start/pricing/")
            );
            assert_eq!(checked_on, Some("2026-09-17"));
        }
    }

    #[test]
    fn gpt_6_astra_template_matches_the_current_official_specification() {
        let (parameters, document, _) = specification(ProviderType::Openai, "gpt-6-astra");
        assert_eq!(parameters.context_window_tokens, known(1_050_000));
        assert_eq!(parameters.max_output_tokens, known(128_000));
        assert_eq!(parameters.reasoning_mode, ModelReasoningMode::Always);
        assert_eq!(
            parameters.default_reasoning_effort,
            Some(ReasoningEffortKey::Medium)
        );
        assert_eq!(parameters.image_input, ModelFeatureSupport::Supported);
        assert_eq!(parameters.tool_calls, ModelFeatureSupport::Supported);
        assert_eq!(
            document,
            Some("https://developers.openai.com/api/docs/models/gpt-6-astra")
        );
    }
}
