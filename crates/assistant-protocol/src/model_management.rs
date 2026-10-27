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
    Sub2api,
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
    /// 选择原生压缩机制；关闭时仍使用本地摘要。
    #[serde(default)]
    pub server_compaction_enabled: bool,
    /// 选择服务端判断触发时机；本版暂不允许开启。
    #[serde(default)]
    pub server_auto_compaction_enabled: bool,
    /// 当前路由的完整 compact 窗口能力，独立于用户开关。
    #[serde(default)]
    pub server_compaction_support: ModelFeatureSupport,
    /// 输入计量是可选增强，不决定压缩机制。
    #[serde(default)]
    pub input_token_count_support: ModelFeatureSupport,
    pub context_window_tokens: ModelTokenLimit,
    pub max_input_tokens: ModelTokenLimit,
    /// 未知时不限制请求输出；上游或用户请求预算决定实际上限。
    pub max_output_tokens: ModelTokenLimit,
    pub reasoning_max_input_tokens: ModelTokenLimit,
    pub reasoning_max_output_tokens: ModelTokenLimit,
    /// Unknown 使用流式缺省支持；Unsupported 仍禁止当前流式执行。
    pub streaming: ModelFeatureSupport,
    pub tool_choice: ModelToolChoiceSupport,
    pub tool_image_projection: ModelToolImageProjection,
    pub image_input: ModelFeatureSupport,
    /// Unknown 使用基础工具调用缺省支持，明确 Unsupported 保留。
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

/// 本地模型目录无法完整恢复时的稳定诊断；不携带原始 JSON 或 Provider 响应。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ModelCatalogDiagnosticCode {
    StoredSnapshotInvalid,
}

/// Provider 最近一次成功显式刷新的本地目录投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ProviderModelCatalogSnapshot {
    pub provider_instance_id: ProviderInstanceId,
    pub models: Vec<DiscoveredModel>,
    pub refreshed_at_ms: Option<i64>,
    pub connection_changed: bool,
    pub diagnostic: Option<ModelCatalogDiagnosticCode>,
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
    Sub2api,
    Opencode,
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
            Self::Sub2api => ModelDiscoveryFormat::Sub2api,
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
    /// 连接级策略；旧持久配置和缺失字段默认关闭。
    #[serde(default)]
    pub server_history_enabled: bool,
    /// None 使用服务商默认；Unknown 不是有效的显式覆盖。
    #[serde(default)]
    pub server_history_support_override: Option<ModelFeatureSupport>,
}

impl ProviderConnection {
    /// 只判断已核实的接口身份；自定义服务地址不继承官方部署能力。
    /// Chat 协议始终不能被能力覆盖提升为 Responses 续接。
    pub fn server_history_support(&self) -> ModelFeatureSupport {
        use ModelFeatureSupport::*;
        let responses = matches!(
            self.provider_type,
            ProviderType::Openai
                | ProviderType::Sub2api
                | ProviderType::Deepseek
                | ProviderType::DashscopeApi
                | ProviderType::DashscopePlan
                | ProviderType::Moonshot
        ) && self.protocol_preference
            != ProviderProtocolPreference::ChatCompletions;
        // OpenCode 首批只验证 HTTP/SSE，显式覆盖也不能启用未接入的续接能力。
        if self.provider_type == ProviderType::Opencode {
            return if self.protocol_preference == ProviderProtocolPreference::Responses
                && self.server_history_support_override != Some(Unsupported)
            {
                Unknown
            } else {
                Unsupported
            };
        }
        if !responses {
            return Unsupported;
        }
        if let Some(value) = self.server_history_support_override {
            return value;
        }
        let endpoint = self.endpoint.trim_end_matches('/');
        match self.provider_type {
            ProviderType::Openai if endpoint == "https://api.openai.com/v1" => Supported,
            ProviderType::DashscopeApi if dashscope_history_endpoint(endpoint) => Supported,
            ProviderType::Deepseek | ProviderType::Moonshot => Unsupported,
            _ => Unknown,
        }
    }
}

// 仅接受官方当前 Responses 路径；workspace 子域不接受用户信息、端口或额外域名。
fn dashscope_history_endpoint(endpoint: &str) -> bool {
    let Some(host) = endpoint
        .strip_prefix("https://")
        .and_then(|rest| rest.strip_suffix("/compatible-mode/v1"))
    else {
        return false;
    };
    if matches!(
        host,
        "dashscope.aliyuncs.com"
            | "dashscope-intl.aliyuncs.com"
            | "dashscope-us.aliyuncs.com"
            | "cn-hongkong.dashscope.aliyuncs.com"
    ) {
        return true;
    }
    let Some((workspace, region)) = host.split_once('.') else {
        return false;
    };
    !workspace.is_empty()
        && workspace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && matches!(
            region,
            "cn-beijing.maas.aliyuncs.com"
                | "ap-southeast-1.maas.aliyuncs.com"
                | "us-east-1.maas.aliyuncs.com"
                | "eu-central-1.maas.aliyuncs.com"
                | "cn-hongkong.maas.aliyuncs.com"
                | "ap-northeast-1.maas.aliyuncs.com"
        )
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
    /// 配置源提供的管理策略与刷新状态；不参与模型选择和参数读取，不持久化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub management: Option<ModelManagementStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ModelManagementStatus {
    pub read_only: bool,
    pub unavailable_reason: Option<String>,
    pub last_success_at_ms: Option<i64>,
    pub last_refresh_error: Option<String>,
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
            Self::XHigh => "xhigh",
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

#[cfg(test)]
mod effort_serialization_tests {
    use super::*;

    #[test]
    fn xhigh_uses_only_the_official_spelling_for_scalar_and_map_keys() {
        assert_eq!(
            ReasoningEffortKey::parse("xhigh"),
            Some(ReasoningEffortKey::XHigh)
        );
        assert_eq!(ReasoningEffortKey::parse("x_high"), None);
        let value: ReasoningEffortKey = serde_json::from_value(serde_json::json!("xhigh")).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), "xhigh");
        assert!(serde_json::from_value::<ReasoningEffortKey>(serde_json::json!("x_high")).is_err());
        let map: BTreeMap<ReasoningEffortKey, String> =
            serde_json::from_value(serde_json::json!({"xhigh": "vendor-value"})).unwrap();
        assert_eq!(
            serde_json::to_value(map).unwrap(),
            serde_json::json!({"xhigh": "vendor-value"})
        );
        assert!(
            serde_json::from_value::<BTreeMap<ReasoningEffortKey, String>>(
                serde_json::json!({"x_high": "vendor-value"})
            )
            .is_err()
        );
        assert_eq!(ReasoningEffortKey::XHigh.as_str(), "xhigh");
    }
}
