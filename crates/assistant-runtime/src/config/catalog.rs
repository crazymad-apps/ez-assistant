//! 随包模型目录及 Provider-neutral capability 编译。
//!
//! 目录只接受受限数据：精确三元组、布尔能力、稳定 effort key、展示 label 和受限
//! wire value。它不能声明字段名、JSON path、请求片段或脚本。

use std::collections::{BTreeMap, BTreeSet};

use agent_types::ProviderId;
use serde::Deserialize;
use thiserror::Error;

use super::domain::ModelProtocol;

const SUPPORTED_CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 产品稳定的 reasoning effort key；显示文本与线上值均不参与跨模型排序。
pub enum ReasoningEffortKey {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffortKey {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 目录可交给协议 Adapter 的受限 effort 值。
pub enum ReasoningEffortWireValue {
    String(String),
    PositiveInteger(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一个已校验的 effort 选项。
pub struct ResolvedReasoningEffort {
    pub key: ReasoningEffortKey,
    pub label: String,
    pub wire_value: ReasoningEffortWireValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 已编译的 reasoning 能力；effort 为空表示模型有 thinking 但没有强度概念。
pub struct ResolvedReasoningCapability {
    pub efforts: Vec<ResolvedReasoningEffort>,
    pub default_effort: Option<ReasoningEffortKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Runtime 唯一编译入口产生的模型能力事实。
pub struct ResolvedModelCapabilities {
    pub image_input: bool,
    pub reasoning: Option<ResolvedReasoningCapability>,
    pub tool_calls: bool,
    pub streaming: bool,
}

impl ResolvedModelCapabilities {
    /// 当前 OpenAI Chat Completions Adapter 的保守能力基线。
    pub fn conservative_openai_chat_completions() -> Self {
        Self {
            image_input: false,
            reasoning: None,
            tool_calls: true,
            streaming: true,
        }
    }

    pub fn reasoning_enabled(&self) -> bool {
        self.reasoning.is_some()
    }
}

#[derive(Debug, Error)]
/// 随包目录不可安全使用；错误不包含目录正文。
pub enum ModelCatalogError {
    #[error("model catalog is not valid JSON")]
    InvalidJson,
    #[error("model catalog schema version is not supported")]
    UnsupportedSchemaVersion,
    #[error("model catalog metadata is invalid")]
    InvalidMetadata,
    #[error("model catalog contains an unsupported protocol")]
    UnsupportedProtocol,
    #[error("model catalog contains an invalid exact route")]
    InvalidRoute,
    #[error("model catalog contains a duplicate route")]
    DuplicateRoute,
    #[error("model catalog contains invalid capabilities")]
    InvalidCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 随包目录中的一组精确模型路由及其表单展示文本。
pub struct ModelCatalogRoute {
    pub provider: ProviderId,
    pub provider_label: String,
    pub protocol: ModelProtocol,
    pub protocol_label: String,
    pub model_ids: Vec<String>,
}

#[derive(Clone, Default)]
/// 经过完整校验的只读模型目录。
pub struct ModelCatalog {
    revision: String,
    entries: BTreeMap<(ProviderId, ModelProtocol, String), ResolvedModelCapabilities>,
    routes: Vec<ModelCatalogRoute>,
}

impl ModelCatalog {
    pub fn empty() -> Self {
        Self::default()
    }

    /// 严格解析随包 JSON；未知字段、模糊别名和重复三元组全部拒绝。
    pub fn from_json(document: &str) -> Result<Self, ModelCatalogError> {
        let raw: RawCatalog =
            serde_json::from_str(document).map_err(|_| ModelCatalogError::InvalidJson)?;
        if raw.schema_version != SUPPORTED_CATALOG_SCHEMA_VERSION {
            return Err(ModelCatalogError::UnsupportedSchemaVersion);
        }
        if raw.catalog_revision.trim().is_empty() || raw.catalog_revision.len() > 64 {
            return Err(ModelCatalogError::InvalidMetadata);
        }

        let mut entries = BTreeMap::new();
        let mut routes = Vec::with_capacity(raw.models.len());
        for entry in raw.models {
            if !valid_provider_id(&entry.provider) {
                return Err(ModelCatalogError::InvalidRoute);
            }
            let provider_label = compile_label(entry.provider_label.as_deref(), &entry.provider)?;
            let provider = ProviderId::new(entry.provider.clone())
                .map_err(|_| ModelCatalogError::InvalidRoute)?;
            let protocol = ModelProtocol::parse_catalog(&entry.protocol)
                .ok_or(ModelCatalogError::UnsupportedProtocol)?;
            let protocol_label = compile_label(entry.protocol_label.as_deref(), &entry.protocol)?;
            if entry.model_ids.is_empty() {
                return Err(ModelCatalogError::InvalidRoute);
            }
            let capabilities = compile_capabilities(
                &entry.capabilities,
                &ResolvedModelCapabilities::conservative_openai_chat_completions(),
            )
            .map_err(|_| ModelCatalogError::InvalidCapabilities)?;
            let mut aliases = BTreeSet::new();
            for model_id in &entry.model_ids {
                if !valid_exact_model_id(model_id) || !aliases.insert(model_id.clone()) {
                    return Err(ModelCatalogError::InvalidRoute);
                }
                if entries
                    .insert(
                        (provider.clone(), protocol, model_id.clone()),
                        capabilities.clone(),
                    )
                    .is_some()
                {
                    return Err(ModelCatalogError::DuplicateRoute);
                }
            }
            routes.push(ModelCatalogRoute {
                provider,
                provider_label,
                protocol,
                protocol_label,
                model_ids: entry.model_ids,
            });
        }
        Ok(Self {
            revision: raw.catalog_revision,
            entries,
            routes,
        })
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn routes(&self) -> &[ModelCatalogRoute] {
        &self.routes
    }

    pub fn resolve(
        &self,
        provider: &ProviderId,
        protocol: ModelProtocol,
        model_id: &str,
    ) -> ResolvedModelCapabilities {
        self.entries
            .get(&(provider.clone(), protocol, model_id.to_owned()))
            .cloned()
            .unwrap_or_else(ResolvedModelCapabilities::conservative_openai_chat_completions)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    schema_version: u32,
    catalog_revision: String,
    models: Vec<RawCatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogEntry {
    provider: String,
    #[serde(default)]
    provider_label: Option<String>,
    protocol: String,
    #[serde(default)]
    protocol_label: Option<String>,
    model_ids: Vec<String>,
    capabilities: CapabilityInput,
}

fn compile_label(candidate: Option<&str>, fallback: &str) -> Result<String, ModelCatalogError> {
    let label = candidate.unwrap_or(fallback);
    if label.trim().is_empty() || label.len() > 64 {
        return Err(ModelCatalogError::InvalidMetadata);
    }
    Ok(label.to_owned())
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct CapabilityInput {
    pub(super) image_input: Option<bool>,
    pub(super) reasoning: Option<ReasoningInput>,
    pub(super) tool_calls: Option<bool>,
    pub(super) streaming: Option<bool>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReasoningInput {
    enabled: bool,
    default_effort: Option<String>,
    #[serde(default)]
    effort_map: BTreeMap<String, EffortInput>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffortInput {
    label: String,
    wire_value: RawWireValue,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum RawWireValue {
    String(String),
    Integer(i64),
}

#[derive(Debug)]
pub(super) struct CapabilityValidationError;

/// capability override 的普通字段覆盖上层来源；reasoning 一旦出现便整块替换。
pub(super) fn compile_capabilities(
    input: &CapabilityInput,
    base: &ResolvedModelCapabilities,
) -> Result<ResolvedModelCapabilities, CapabilityValidationError> {
    let reasoning = match input.reasoning.as_ref() {
        Some(reasoning) => compile_reasoning(reasoning)?,
        None => base.reasoning.clone(),
    };
    Ok(ResolvedModelCapabilities {
        image_input: input.image_input.unwrap_or(base.image_input),
        reasoning,
        tool_calls: input.tool_calls.unwrap_or(base.tool_calls),
        streaming: input.streaming.unwrap_or(base.streaming),
    })
}

fn compile_reasoning(
    input: &ReasoningInput,
) -> Result<Option<ResolvedReasoningCapability>, CapabilityValidationError> {
    if !input.enabled {
        if input.default_effort.is_some() || !input.effort_map.is_empty() {
            return Err(CapabilityValidationError);
        }
        return Ok(None);
    }

    let mut efforts = Vec::with_capacity(input.effort_map.len());
    let mut wire_values = BTreeSet::new();
    for (raw_key, raw_effort) in &input.effort_map {
        let key = ReasoningEffortKey::parse(raw_key).ok_or(CapabilityValidationError)?;
        if raw_effort.label.trim().is_empty() || raw_effort.label.len() > 64 {
            return Err(CapabilityValidationError);
        }
        let wire_value = match &raw_effort.wire_value {
            RawWireValue::String(value) if !value.trim().is_empty() && value.len() <= 64 => {
                ReasoningEffortWireValue::String(value.clone())
            }
            RawWireValue::Integer(value) if *value > 0 => {
                ReasoningEffortWireValue::PositiveInteger(
                    u64::try_from(*value).map_err(|_| CapabilityValidationError)?,
                )
            }
            _ => return Err(CapabilityValidationError),
        };
        if !wire_values.insert(wire_value.clone()) {
            return Err(CapabilityValidationError);
        }
        efforts.push(ResolvedReasoningEffort {
            key,
            label: raw_effort.label.clone(),
            wire_value,
        });
    }
    efforts.sort_by_key(|effort| effort.key);

    let default_effort = match input.default_effort.as_deref() {
        Some(value) => {
            let key = ReasoningEffortKey::parse(value).ok_or(CapabilityValidationError)?;
            if !efforts.iter().any(|effort| effort.key == key) {
                return Err(CapabilityValidationError);
            }
            Some(key)
        }
        None if efforts.is_empty() => None,
        None => return Err(CapabilityValidationError),
    };
    Ok(Some(ResolvedReasoningCapability {
        efforts,
        default_effort,
    }))
}

fn valid_provider_id(provider: &str) -> bool {
    let bytes = provider.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_exact_model_id(model_id: &str) -> bool {
    !model_id.trim().is_empty()
        && model_id.trim() == model_id
        && model_id.len() <= 128
        && !model_id
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(capabilities: &str) -> String {
        format!(
            r#"{{
                "schema_version": 1,
                "catalog_revision": "fixture",
                "models": [{{
                    "provider": "fixture",
                    "protocol": "openai_chat_completions",
                    "model_ids": ["fixture-model"],
                    "capabilities": {capabilities}
                }}]
            }}"#
        )
    }

    #[test]
    fn compiles_string_and_integer_efforts_in_stable_order() {
        let document = catalog(
            r#"{
                "reasoning": {
                    "enabled": true,
                    "default_effort": "max",
                    "effort_map": {
                        "max": {"label": "Max", "wire_value": 4096},
                        "low": {"label": "Low", "wire_value": "low"}
                    }
                }
            }"#,
        );
        let catalog = ModelCatalog::from_json(&document).expect("catalog");
        let capabilities = catalog.resolve(
            &ProviderId::new("fixture").expect("provider"),
            ModelProtocol::OpenAiChatCompletions,
            "fixture-model",
        );
        let reasoning = capabilities.reasoning.expect("reasoning");
        assert_eq!(reasoning.default_effort, Some(ReasoningEffortKey::Max));
        assert_eq!(reasoning.efforts[0].key, ReasoningEffortKey::Low);
        assert_eq!(reasoning.efforts[1].key, ReasoningEffortKey::Max);
    }

    #[test]
    fn rejects_unknown_fields_duplicate_routes_and_patterns() {
        assert!(matches!(
            ModelCatalog::from_json(&catalog(r#"{"unknown": true}"#)),
            Err(ModelCatalogError::InvalidJson)
        ));
        let duplicate = catalog("{}").replace(
            "[\"fixture-model\"]",
            "[\"fixture-model\", \"fixture-model\"]",
        );
        assert!(matches!(
            ModelCatalog::from_json(&duplicate),
            Err(ModelCatalogError::InvalidRoute)
        ));
        let pattern = catalog("{}").replace("fixture-model", "fixture-*");
        assert!(matches!(
            ModelCatalog::from_json(&pattern),
            Err(ModelCatalogError::InvalidRoute)
        ));

        let duplicate_route = catalog("{}").replace(
            "]\n            }",
            r#",{
                    "provider": "fixture",
                    "protocol": "openai_chat_completions",
                    "model_ids": ["fixture-model"],
                    "capabilities": {}
                }]
            }"#,
        );
        assert!(matches!(
            ModelCatalog::from_json(&duplicate_route),
            Err(ModelCatalogError::DuplicateRoute)
        ));
    }

    #[test]
    fn rejects_invalid_effort_combinations() {
        let duplicate_wire = catalog(
            r#"{"reasoning":{"enabled":true,"default_effort":"high","effort_map":{
                "low":{"label":"Low","wire_value":"same"},
                "high":{"label":"High","wire_value":"same"}
            }}}"#,
        );
        assert!(matches!(
            ModelCatalog::from_json(&duplicate_wire),
            Err(ModelCatalogError::InvalidCapabilities)
        ));
        let dangling = catalog(
            r#"{"reasoning":{"enabled":true,"default_effort":"max","effort_map":{
                "high":{"label":"High","wire_value":"high"}
            }}}"#,
        );
        assert!(matches!(
            ModelCatalog::from_json(&dangling),
            Err(ModelCatalogError::InvalidCapabilities)
        ));
    }

    #[test]
    fn unknown_model_uses_conservative_protocol_baseline() {
        let catalog = ModelCatalog::from_json(&catalog("{}")).expect("catalog");
        let capabilities = catalog.resolve(
            &ProviderId::new("fixture").expect("provider"),
            ModelProtocol::OpenAiChatCompletions,
            "new-model",
        );
        assert!(!capabilities.image_input);
        assert!(!capabilities.reasoning_enabled());
        assert!(capabilities.tool_calls);
        assert!(capabilities.streaming);
    }
}
