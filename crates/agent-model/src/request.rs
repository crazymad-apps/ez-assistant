use std::collections::BTreeMap;

use agent_types::{ConversationSnapshot, ToolChoice, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 已完成渲染并按顺序冻结的 System Prompt Parts。
///
/// 本类型只保存模型可见的最终文本，不保存构建配置或结构化业务数据。公开接口只允许
/// 读取、克隆或消费整个快照，避免执行期间原地修改其中的 Part。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SystemPromptSnapshot {
    parts: Vec<String>,
}

impl SystemPromptSnapshot {
    /// 使用已经按最终顺序渲染完成的 Parts 创建快照。
    pub fn new(parts: Vec<String>) -> Self {
        Self { parts }
    }

    /// 按冻结顺序只读访问全部 Parts。
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    /// 消费快照并取回其中的 Parts。
    pub fn into_parts(self) -> Vec<String> {
        self.parts
    }

    /// 快照是否不包含任何 System Prompt Part。
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 一次 Provider Turn 的完整规范请求。
///
/// 请求只包含影响模型语义的输入；调用目标（endpoint、credential、模型名）属于
/// 服务实例的构造参数，业务 `RunId` 和应用 Session 等控制面信息也不得出现在
/// 这里，它们属于 Adapter 构造参数或 [`crate::ModelCallContext`]。请求交给哪个
/// 服务实例，由调用方按配置编译结果直接决定，请求自身不携带路由信息。
pub struct ModelRequest {
    /// 有序且冻结的 System Prompt。
    pub system: SystemPromptSnapshot,
    /// 规范历史对话。
    pub conversation: ConversationSnapshot,
    /// 模型可见的工具定义。
    pub tools: Vec<ToolDefinition>,
    /// 本次调用的工具选择策略。
    pub tool_choice: ToolChoice,
    /// generation 配置；`None` 字段表示沿用 Provider 默认值。
    pub generation: GenerationConfig,
    /// reasoning 配置；`None` 表示本次调用不请求 reasoning。
    pub reasoning: Option<ReasoningConfig>,
    /// 命名空间隔离的 Provider 私有选项。
    pub provider_options: ProviderOptions,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// 与 Provider 无关的 generation 配置。
///
/// 某个字段是否生效由协议 Adapter 依据已编译能力决定；模型层不做数值范围校验。
pub struct GenerationConfig {
    /// 采样温度。
    pub temperature: Option<f32>,
    /// nucleus 采样阈值。
    pub top_p: Option<f32>,
    /// 最大输出 token 数。
    pub max_output_tokens: Option<u32>,
    /// 有序 stop 序列。
    pub stop: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// reasoning/thinking 模式的配置。
pub struct ReasoningConfig {
    /// 期望的 reasoning 强度；`None` 表示沿用 Provider 默认值。
    pub effort: Option<ReasoningEffort>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// reasoning 强度档位。
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
            Self::XHigh => 3,
            Self::Max => 4,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// ProviderOptions 不满足边界约束时返回的错误。
pub enum ProviderOptionsError {
    #[error("provider options namespace must not be empty")]
    EmptyNamespace,
    #[error("provider options for namespace `{namespace}` must be a JSON object")]
    NotAnObject { namespace: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
/// 按命名空间隔离的 Provider 私有选项。
///
/// 每个命名空间（约定为 Provider 或 Provider 协议标识）对应一个 JSON 对象，
/// 由对应 Adapter 校验和解释；调用方不得依赖把任意字段静默透传给 Provider。
/// 反序列化与 [`ProviderOptions::insert`] 走同一套不变量校验（非空命名空间、
/// 对象值），非法输入在反序列化期失败而不是绕过校验进入请求。
pub struct ProviderOptions {
    namespaces: BTreeMap<String, Value>,
}

impl<'de> Deserialize<'de> for ProviderOptions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let namespaces = BTreeMap::<String, Value>::deserialize(deserializer)?;
        let mut options = Self::new();
        for (namespace, value) in namespaces {
            options
                .insert(namespace, value)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(options)
    }
}

impl ProviderOptions {
    /// 创建空的 Provider 选项集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入一个命名空间的选项对象。
    pub fn insert(
        &mut self,
        namespace: impl Into<String>,
        options: Value,
    ) -> Result<(), ProviderOptionsError> {
        let namespace = namespace.into();
        if namespace.trim().is_empty() {
            return Err(ProviderOptionsError::EmptyNamespace);
        }
        if !options.is_object() {
            return Err(ProviderOptionsError::NotAnObject { namespace });
        }
        self.namespaces.insert(namespace, options);
        Ok(())
    }

    /// 读取一个命名空间的选项对象。
    pub fn get(&self, namespace: &str) -> Option<&Value> {
        self.namespaces.get(namespace)
    }

    /// 是否没有任何命名空间选项。
    pub fn is_empty(&self) -> bool {
        self.namespaces.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{
        ConversationMessage, MessageId, PartId, TextPart, ToolName, UserMessage, UserPart,
    };

    use super::*;

    fn sample_request() -> ModelRequest {
        let conversation =
            ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
                origin: Default::default(),
                transcript_visibility: Default::default(),
                id: MessageId::new("message_1").expect("valid message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("text_1").expect("valid part id"),
                    text: "What date is it?".to_owned(),
                })],
            })]);
        let mut provider_options = ProviderOptions::new();
        provider_options
            .insert(
                "deepseek",
                serde_json::json!({"thinking": {"type": "enabled"}}),
            )
            .expect("valid provider options");
        ModelRequest {
            system: SystemPromptSnapshot::new(vec!["You are a helpful assistant.".to_owned()]),
            conversation,
            tools: vec![ToolDefinition {
                name: ToolName::new("get_date").expect("valid tool name"),
                description: "Get the current date.".to_owned(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig {
                temperature: None,
                top_p: None,
                max_output_tokens: Some(1024),
                stop: vec![],
            },
            reasoning: Some(ReasoningConfig {
                effort: Some(ReasoningEffort::High),
            }),
            provider_options,
        }
    }

    #[test]
    fn model_request_round_trips_without_provider_leaks() {
        let request = sample_request();
        let json = serde_json::to_string(&request).expect("serialize request");
        assert_eq!(
            serde_json::from_str::<ModelRequest>(&json).expect("deserialize request"),
            request
        );
    }

    #[test]
    fn system_prompt_snapshot_is_ordered_transparent_and_read_only() {
        let snapshot = SystemPromptSnapshot::new(vec![
            "base instructions".to_owned(),
            "pinned memory".to_owned(),
        ]);

        assert_eq!(
            snapshot.parts(),
            &["base instructions".to_owned(), "pinned memory".to_owned()]
        );
        assert!(!snapshot.is_empty());
        assert_eq!(
            serde_json::to_value(&snapshot).expect("serialize snapshot"),
            serde_json::json!(["base instructions", "pinned memory"])
        );
        assert_eq!(
            serde_json::from_value::<SystemPromptSnapshot>(serde_json::json!([
                "base instructions",
                "pinned memory"
            ]))
            .expect("deserialize snapshot"),
            snapshot
        );
        assert_eq!(
            snapshot.clone().into_parts(),
            vec!["base instructions".to_owned(), "pinned memory".to_owned()]
        );
        assert!(SystemPromptSnapshot::default().is_empty());
    }

    #[test]
    fn provider_options_are_namespaced_and_validated() {
        let mut options = ProviderOptions::new();
        assert!(options.is_empty());
        assert!(options.insert("  ", serde_json::json!({})).is_err());
        assert!(options.insert("deepseek", serde_json::json!(1)).is_err());
        options
            .insert(
                "deepseek",
                serde_json::json!({"thinking": {"type": "enabled"}}),
            )
            .expect("valid provider options");
        assert!(!options.is_empty());
        assert_eq!(
            options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
        assert_eq!(options.get("openai"), None);
    }

    #[test]
    fn provider_options_deserialization_reuses_insert_validation() {
        // 非对象值在反序列化期失败，不能绕过 insert 校验进入请求。
        let error = serde_json::from_str::<ProviderOptions>(r#"{"deepseek": 1}"#)
            .expect_err("non-object namespace value must fail");
        assert!(error.to_string().contains("must be a JSON object"));

        // 空命名空间同样失败。
        let error = serde_json::from_str::<ProviderOptions>(r#"{"": {}}"#)
            .expect_err("empty namespace must fail");
        assert!(error.to_string().contains("must not be empty"));

        // 合法输入正常反序列化。
        let options: ProviderOptions =
            serde_json::from_str(r#"{"deepseek": {"thinking": {"type": "enabled"}}}"#)
                .expect("valid provider options");
        assert_eq!(
            options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
    }
}
