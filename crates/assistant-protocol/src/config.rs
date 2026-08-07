//! 可安全跨进程展示的 Runtime 配置状态与模型投影。

use serde::{Deserialize, Serialize};

use crate::ModelKey;

/// 当前配置源可供 Runtime 使用的程度。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationState {
    /// 配置文件不存在。
    Missing,
    /// 配置无法形成有效快照。
    Invalid,
    /// 已形成快照，但部分模型或默认模型不可用。
    Degraded,
    /// 默认模型与全部模型均有效。
    Ready,
}

/// 配置诊断的稳定、脱敏分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationIssueCode {
    /// TOML 语法或重复字段错误。
    InvalidSyntax,
    /// schema version 不受当前 Runtime 支持。
    UnsupportedSchemaVersion,
    /// 顶层或全局配置无法解释。
    InvalidTopLevel,
    /// 配置文件类型、权限或大小无法安全处理。
    UnsafeConfigSource,
    /// 配置文件无法读取或解码。
    ConfigReadFailed,
    /// 存在当前 schema 未定义的字段。
    UnknownField,
    /// 必填字段缺失。
    MissingField,
    /// model key 不符合稳定形式约束。
    InvalidModelKey,
    /// 单条模型配置无法解释。
    InvalidModel,
    /// 模型协议不受当前 Runtime 支持。
    UnsupportedProtocol,
    /// provider key 不符合约束。
    InvalidProvider,
    /// endpoint 不符合安全 URL 规则。
    InvalidEndpoint,
    /// API Key 缺失或为空。
    MissingCredential,
    /// token、超时或执行上限无效。
    InvalidLimit,
    /// Runtime 或 Agent 全局策略无效。
    InvalidPolicy,
    /// Provider Profile 与请求参数组合不兼容。
    UnsupportedProfileCombination,
    /// 默认 model key 缺失或当前不可用。
    DefaultModelUnavailable,
}

/// 一条不包含原始 TOML、credential 或底层错误正文的诊断。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfigurationIssue {
    /// 稳定诊断分类。
    pub code: ConfigurationIssueCode,
    /// 可安全展示且合法的关联 model key。
    pub model_key: Option<ModelKey>,
    /// 已脱敏、可直接展示的诊断文本。
    pub message: String,
}

/// 配置总体状态；模型明细通过独立命令查询。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfigurationStatus {
    /// Host 允许展示的配置文件路径；抽象测试源可以没有路径。
    pub config_path: Option<String>,
    /// 当前配置可用程度。
    pub state: ConfigurationState,
    /// 成功读取到的 schema version。
    pub schema_version: Option<u32>,
    /// 形式合法的默认 model key；仍可能暂时不可用。
    pub default_model: Option<ModelKey>,
    /// 不归属于单个合法 model key 的全局诊断。
    pub issues: Vec<ConfigurationIssue>,
}

/// 单条模型配置的脱敏投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelConfiguration {
    /// 非法配置表 key 不会原样跨层展示，因此可能为 None。
    pub model_key: Option<ModelKey>,
    /// 用户可见名称或安全占位名称。
    pub display_name: String,
    /// 通过安全解析的协议名称。
    pub protocol: Option<String>,
    /// 通过安全解析的实际供应商标识。
    pub provider: Option<String>,
    /// 通过 userinfo/query/fragment 校验的 endpoint。
    pub endpoint: Option<String>,
    /// Provider 模型名称。
    pub model: Option<String>,
    /// 模型声明的上下文窗口硬上限。
    pub context_window_tokens: Option<u64>,
    /// 模型声明的单轮输出硬上限。
    pub max_output_tokens: Option<u32>,
    /// Agent 全局请求的单轮输出上限。
    pub agent_max_output_tokens: Option<u32>,
    /// 模型与 Agent 两个输出上限取最小值后的结果。
    pub effective_max_output_tokens: Option<u32>,
    /// 只代表 credential 已通过本地非空校验，不代表 Provider 已验证。
    pub api_key_configured: bool,
    /// 是否对应当前默认 model key。
    pub is_default: bool,
    /// 是否可进入当前有效模型快照。
    pub is_valid: bool,
    /// 本模型的安全诊断。
    pub issues: Vec<ConfigurationIssue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_projection_round_trips_without_a_credential_field() {
        let projection = ModelConfiguration {
            model_key: Some(ModelKey::new("deepseek-chat").expect("model key")),
            display_name: "DeepSeek Chat".to_owned(),
            protocol: Some("chat_completions".to_owned()),
            provider: Some("deepseek".to_owned()),
            endpoint: Some("https://api.example.test/v1".to_owned()),
            model: Some("deepseek-chat".to_owned()),
            context_window_tokens: Some(128_000),
            max_output_tokens: Some(8_192),
            agent_max_output_tokens: Some(4_096),
            effective_max_output_tokens: Some(4_096),
            api_key_configured: true,
            is_default: true,
            is_valid: true,
            issues: Vec::new(),
        };
        let value = serde_json::to_value(&projection).expect("serialize");
        assert!(value.get("api_key").is_none());
        assert!(value.get("credential").is_none());
        assert_eq!(
            serde_json::from_value::<ModelConfiguration>(value).expect("deserialize"),
            projection
        );
    }

    #[test]
    fn every_state_and_issue_code_has_stable_snake_case() {
        let states = [
            (ConfigurationState::Missing, "missing"),
            (ConfigurationState::Invalid, "invalid"),
            (ConfigurationState::Degraded, "degraded"),
            (ConfigurationState::Ready, "ready"),
        ];
        for (state, expected) in states {
            assert_eq!(
                serde_json::to_string(&state).expect("serialize state"),
                format!("\"{expected}\"")
            );
        }

        let codes = [
            (ConfigurationIssueCode::InvalidSyntax, "invalid_syntax"),
            (
                ConfigurationIssueCode::UnsupportedSchemaVersion,
                "unsupported_schema_version",
            ),
            (ConfigurationIssueCode::InvalidTopLevel, "invalid_top_level"),
            (
                ConfigurationIssueCode::UnsafeConfigSource,
                "unsafe_config_source",
            ),
            (
                ConfigurationIssueCode::ConfigReadFailed,
                "config_read_failed",
            ),
            (ConfigurationIssueCode::UnknownField, "unknown_field"),
            (ConfigurationIssueCode::MissingField, "missing_field"),
            (ConfigurationIssueCode::InvalidModelKey, "invalid_model_key"),
            (ConfigurationIssueCode::InvalidModel, "invalid_model"),
            (
                ConfigurationIssueCode::UnsupportedProtocol,
                "unsupported_protocol",
            ),
            (ConfigurationIssueCode::InvalidProvider, "invalid_provider"),
            (ConfigurationIssueCode::InvalidEndpoint, "invalid_endpoint"),
            (
                ConfigurationIssueCode::MissingCredential,
                "missing_credential",
            ),
            (ConfigurationIssueCode::InvalidLimit, "invalid_limit"),
            (ConfigurationIssueCode::InvalidPolicy, "invalid_policy"),
            (
                ConfigurationIssueCode::UnsupportedProfileCombination,
                "unsupported_profile_combination",
            ),
            (
                ConfigurationIssueCode::DefaultModelUnavailable,
                "default_model_unavailable",
            ),
        ];
        for (code, expected) in codes {
            assert_eq!(
                serde_json::to_string(&code).expect("serialize code"),
                format!("\"{expected}\"")
            );
        }
    }
}
