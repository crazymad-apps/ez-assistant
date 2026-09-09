//! 服务商实例、在线模型与用户固定参数的应用契约；不包含具体 Adapter 或数据库依赖。
#[cfg(test)]
mod tests;

use crate::{ProviderInstanceId, ReasoningEffortKey, SecretValue, SessionId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, num::NonZeroU64};
use ts_rs::TS;

/// 实例级在线接口格式；它不决定推理协议，也不按模型名选择。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelDiscoveryFormat {
    #[serde(rename = "openai")]
    OpenAi,
    Vllm,
    Moonshot,
    #[serde(rename = "dashscope_native")]
    DashScope,
}

/// 模型能否关闭思考；不与 Provider 的字段编码规则混用。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelReasoningMode {
    #[default]
    Unknown,
    Unsupported,
    Optional,
    Always,
}

/// 标准化 Token 限制；必需限制未知或非法时不能用于构造执行规格。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "state", content = "value")]
pub enum ModelTokenLimit {
    /// 缺失或 null。null 可能表示不适用或无限制，不能擅自补成一个有限上限。
    #[default]
    Unknown,
    Known(NonZeroU64),
    /// 接口给出了非正整数、溢出值或其他无法解释的形状。
    Invalid,
}

/// 单项能力的已知程度；不能把服务商未声明的能力当作支持或不支持。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelFeatureSupport {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

/// 在线预填或用户固定的标准化参数；不包含方言、凭据或静态目录回退。
///
/// 思考模式限制单独保留，避免用普通输出上限覆盖服务商为该模式报告的限制。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelParameters {
    pub context_window_tokens: ModelTokenLimit,
    pub max_input_tokens: ModelTokenLimit,
    pub max_output_tokens: ModelTokenLimit,
    pub reasoning_max_input_tokens: ModelTokenLimit,
    pub reasoning_max_output_tokens: ModelTokenLimit,
    pub streaming: ModelFeatureSupport,
    pub tool_choice: ModelToolChoiceSupport,
    pub tool_image_projection: ModelToolImageProjection,
    pub image_input: ModelFeatureSupport,
    pub tool_calls: ModelFeatureSupport,
    pub reasoning: ModelFeatureSupport,
    pub reasoning_mode: ModelReasoningMode,
    pub reasoning_efforts: Option<BTreeMap<ReasoningEffortKey, String>>,
    pub default_reasoning_effort: Option<ReasoningEffortKey>,
}

/// 单个在线目录条目；目录中存在不等于已获得运行必需的全部参数或账号调用权限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DiscoveredModel {
    /// Runtime 结合模板计算的参数状态；不改变原始在线元数据。
    #[serde(default)]
    pub configuration: Option<ModelConfigurationSummary>,
    pub model_id: String,
    pub display_name: Option<String>,
    pub metadata: ModelParameters,
}

/// 二元模型身份，model_id 保留服务商原值，不拼接成配置 key。
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelSelection {
    pub provider_instance_id: ProviderInstanceId,
    pub model_id: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ProviderProtocolPreference {
    #[default]
    Auto,
    Responses,
    ChatCompletions,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ProviderType {
    Openai,
    Deepseek,
    DashscopeApi,
    DashscopePlan,
    Moonshot,
    Zhipu,
    Vllm,
    Local,
}

impl ProviderType {
    /// 类型确定发现格式；自定义路径不改变返回数据的解析约定。
    pub const fn discovery_format(self) -> ModelDiscoveryFormat {
        match self {
            Self::DashscopeApi => ModelDiscoveryFormat::DashScope,
            Self::Moonshot => ModelDiscoveryFormat::Moonshot,
            Self::Vllm => ModelDiscoveryFormat::Vllm,
            _ => ModelDiscoveryFormat::OpenAi,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderConnection {
    pub display_name: String,
    pub provider_type: ProviderType,
    pub endpoint: String,
    pub protocol_preference: ProviderProtocolPreference,
    pub models_path: String,
    pub discovery_format: ModelDiscoveryFormat,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ProviderCredentialChange {
    Unchanged,
    Replace(#[ts(type = "string")] SecretValue),
    Clear,
}

/// 脱敏连接视图；普通查询不含 API Key。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderSummary {
    pub provider_instance_id: ProviderInstanceId,
    pub connection: ProviderConnection,
    pub has_api_key: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelSettings {
    pub default_model: Option<ModelSelection>,
    pub vision_model: Option<ModelSelection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelFixedConfig {
    #[serde(default)]
    pub origin: ModelConfigOrigin,
    pub selection: ModelSelection,
    pub parameters: ModelParameters,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelToolChoiceSupport {
    pub auto: ModelFeatureSupport,
    pub none: ModelFeatureSupport,
    pub required: ModelFeatureSupport,
    pub named: ModelFeatureSupport,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelToolImageProjection {
    #[default]
    Unknown,
    Unsupported,
    NativeToolResult,
    FollowUpUserMessage,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderUsage {
    pub default_model: bool,
    pub vision_model: bool,
    pub session_count: u64,
    pub fixed_config_count: u64,
    /// 最多 20 条；包含未加载、归档会话，计数不受此摘要限制。
    pub sessions: Vec<ProviderSessionUsage>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderSessionUsage {
    pub session_id: SessionId,
    pub title: String,
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
            Self::XHigh => "x_high",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CreateProviderRequest {
    pub connection: ProviderConnection,
    pub credential: ProviderCredentialChange,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateProviderRequest {
    pub provider_instance_id: ProviderInstanceId,
    pub connection: ProviderConnection,
    pub credential: ProviderCredentialChange,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderRequest {
    pub provider_instance_id: ProviderInstanceId,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListFixedModelConfigsRequest {
    pub provider_instance_id: ProviderInstanceId,
    pub offset: u32,
    pub limit: u32,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SaveModelFixedConfigRequest {
    #[serde(default)]
    pub origin: ModelConfigOrigin,
    pub selection: ModelSelection,
    pub parameters: ModelParameters,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelConfigurationSource {
    Fixed,
    Online,
    Template,
    Unconfigured,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelConfigurationDetail {
    pub origin: ModelConfigOrigin,
    pub selection: ModelSelection,
    pub parameters: ModelParameters,
    pub source: ModelConfigurationSource,
    pub updated_at_ms: Option<i64>,
    pub field_sources: BTreeMap<String, ModelConfigurationSource>,
    pub template_document: Option<String>,
    pub template_checked_on: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListProvidersRequest {}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetModelSettingsRequest {}

/// 模型记录的创建来源；与字段预填来源不同，保存后不可修改。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelConfigOrigin {
    #[default]
    Online,
    Manual,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetModelConfigurationRequest {
    #[serde(flatten)]
    pub selection: ModelSelection,
    #[serde(default)]
    pub origin: ModelConfigOrigin,
}
impl From<ModelSelection> for GetModelConfigurationRequest {
    fn from(selection: ModelSelection) -> Self {
        Self {
            selection,
            origin: ModelConfigOrigin::Online,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelConfigurationSummary {
    pub uses_template: bool,
    pub requires_configuration: bool,
}
