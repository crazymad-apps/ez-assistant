use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use thiserror::Error;

use crate::{
    FinishReason, MessageId, ModelIdentity, PartId, ProtocolId, ProviderId, TokenUsage, ToolCallId,
    ToolName, ToolResult,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// 发给模型的一份规范对话快照。
///
/// Runtime 负责从持久化会话生成快照；Agent Core 和 Provider Adapter 只消费这个值。
pub struct ConversationSnapshot {
    /// 按发生顺序保存的对话消息。
    pub messages: Vec<ConversationMessage>,
}

impl ConversationSnapshot {
    /// 从有序对话消息创建快照。
    pub fn new(messages: Vec<ConversationMessage>) -> Self {
        Self { messages }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", content = "turn", rename_all = "snake_case")]
/// 规范对话中的一条消息。
///
/// `serde(tag = "role")` 表示序列化为 JSON 时会带有 `role` 字段，
/// 但这里的结构仍然是项目内部协议，不等同于 OpenAI 的原生 message。
pub enum ConversationMessage {
    /// 系统级指令。
    System(SystemMessage),
    /// 用户输入。
    User(UserMessage),
    /// 模型生成的完整响应。
    Assistant(AssistantMessage),
    /// 对某个 Tool Call 的执行结果。
    Tool(ToolMessage),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一条系统指令。
pub struct SystemMessage {
    /// 规范消息 ID。
    pub id: MessageId,
    /// 系统指令正文。
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一次用户输入。
pub struct UserMessage {
    /// 规范消息 ID。
    pub id: MessageId,
    /// 用户消息的有序内容片段，包含用户真实输入与应用注入文本。
    pub parts: Vec<UserPart>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
/// UserMessage 中可能出现的内容片段。
pub enum UserPart {
    /// 用户真实输入的文本。
    Text(TextPart),
    /// 上层应用注入的约束/上下文文本；UI 展示时隐藏，回放时仍进入发给模型的内容。
    Injected(TextPart),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 一次完整的模型响应。
///
/// reasoning、正文和 Tool Call 都保存在同一个有序 `parts` 列表中，
/// 这样 DeepSeek 等 Provider 的下一轮请求可以恢复原始消息结构。
pub struct AssistantMessage {
    /// 规范消息 ID。
    pub id: MessageId,
    /// 生成该响应的模型身份。
    pub model: ModelIdentity,
    /// 按 Provider 输出顺序保存的响应片段。
    pub parts: Vec<AssistantPart>,
    /// Provider 结束本次输出的原因。
    pub finish_reason: FinishReason,
    /// Provider 返回的 token 用量；未提供时为 `None`。
    pub usage: Option<TokenUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
/// AssistantMessage 中可能出现的内容片段。
pub enum AssistantPart {
    /// 模型的 reasoning/thinking 内容。
    Reasoning(ReasoningPart),
    /// 面向用户的普通文本。
    Text(TextPart),
    /// 模型请求执行工具。
    ToolCall(ToolCall),
    /// 无法映射为通用类型、但后续请求必须原样回传的 Provider 状态。
    ProviderState(OpaqueProviderState),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一个 reasoning 内容片段。
pub struct ReasoningPart {
    /// 片段 ID，用于流式事件聚合。
    pub id: PartId,
    /// 已聚合完成的 reasoning 文本。
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一个普通文本片段。
pub struct TextPart {
    /// 片段 ID，用于流式事件聚合。
    pub id: PartId,
    /// 已聚合完成的文本。
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 模型发起的一次工具调用。
pub struct ToolCall {
    /// Provider 分配的调用 ID。
    pub id: ToolCallId,
    /// 要调用的工具名称。
    pub name: ToolName,
    /// 已完成组装和 JSON 解析的工具参数。
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 对一个 Tool Call 的结果消息。
pub struct ToolMessage {
    /// 规范消息 ID。
    pub id: MessageId,
    /// 带有 ToolCallId 的结果，用于和请求严格配对。
    pub result: ToolResult,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// OpaqueProviderState 不满足边界约束时返回的错误。
pub enum ProviderStateError {
    #[error("provider state type must not be empty")]
    EmptyStateType,
    #[error("provider state media type must not be empty")]
    EmptyMediaType,
    #[error("provider state format version must be greater than zero")]
    InvalidFormatVersion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 只能由对应 Provider Adapter 解释的不透明续传状态。
///
/// 普通 reasoning 文本不能放在这里；只有确实无法规范化、但下一次请求必须回传的
/// Provider 私有数据才使用这个类型。
pub struct OpaqueProviderState {
    provider: ProviderId,
    protocol: ProtocolId,
    state_type: String,
    media_type: String,
    format_version: u32,
    payload: Vec<u8>,
}

// 这是 Serde 反序列化时使用的临时结构。它先接住 JSON 字段，随后必须调用
// `OpaqueProviderState::new` 做业务校验，防止外部 JSON 直接构造非法状态。
#[derive(Deserialize)]
struct OpaqueProviderStateWire {
    provider: ProviderId,
    protocol: ProtocolId,
    state_type: String,
    media_type: String,
    format_version: u32,
    payload: Vec<u8>,
}

impl<'de> Deserialize<'de> for OpaqueProviderState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OpaqueProviderStateWire::deserialize(deserializer)?;
        Self::new(
            wire.provider,
            wire.protocol,
            wire.state_type,
            wire.media_type,
            wire.format_version,
            wire.payload,
        )
        .map_err(de::Error::custom)
    }
}

impl OpaqueProviderState {
    /// 创建带有明确 Provider、协议和格式版本边界的不透明状态。
    pub fn new(
        provider: ProviderId,
        protocol: ProtocolId,
        state_type: impl Into<String>,
        media_type: impl Into<String>,
        format_version: u32,
        payload: Vec<u8>,
    ) -> Result<Self, ProviderStateError> {
        let state_type = state_type.into();
        if state_type.trim().is_empty() {
            return Err(ProviderStateError::EmptyStateType);
        }
        let media_type = media_type.into();
        if media_type.trim().is_empty() {
            return Err(ProviderStateError::EmptyMediaType);
        }
        if format_version == 0 {
            return Err(ProviderStateError::InvalidFormatVersion);
        }
        Ok(Self {
            provider,
            protocol,
            state_type,
            media_type,
            format_version,
            payload,
        })
    }

    /// 返回拥有该状态的 Provider。
    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    /// 返回该状态所属的 Provider 协议。
    pub fn protocol(&self) -> &ProtocolId {
        &self.protocol
    }

    /// 返回 Provider 定义的状态类型。
    pub fn state_type(&self) -> &str {
        &self.state_type
    }

    /// 返回 payload 的媒体类型，例如 `application/json`。
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// 返回 payload 格式版本，用于未来兼容迁移。
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// 以只读字节切片形式返回不透明 payload。
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolResultContent, ToolResultStatus};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        value.to_owned().try_into().expect("valid id")
    }

    #[test]
    fn mixed_assistant_parts_preserve_order() {
        let turn = AssistantMessage {
            id: id("turn_1"),
            model: ModelIdentity::new(id("deepseek"), "deepseek-reasoner"),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: id("reasoning_1"),
                    text: "Need the date first".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: id("text_1"),
                    text: "Let me check".to_owned(),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: id("call_1"),
                    name: ToolName::new("get_date").expect("valid tool name"),
                    arguments: serde_json::json!({}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        let json = serde_json::to_string(&turn).expect("serialize turn");
        let decoded: AssistantMessage = serde_json::from_str(&json).expect("deserialize turn");
        assert_eq!(decoded, turn);
        assert!(matches!(decoded.parts[0], AssistantPart::Reasoning(_)));
        assert!(matches!(decoded.parts[1], AssistantPart::Text(_)));
        assert!(matches!(decoded.parts[2], AssistantPart::ToolCall(_)));
    }

    #[test]
    fn conversation_round_trips_tool_call_and_result() {
        let snapshot = ConversationSnapshot::new(vec![
            ConversationMessage::User(UserMessage {
                id: id("message_1"),
                parts: vec![UserPart::Text(TextPart {
                    id: id("text_1"),
                    text: "What date is it?".to_owned(),
                })],
            }),
            ConversationMessage::Assistant(AssistantMessage {
                id: id("turn_1"),
                model: ModelIdentity::new(id("deepseek"), "deepseek-reasoner"),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: id("call_1"),
                    name: ToolName::new("get_date").expect("valid tool name"),
                    arguments: serde_json::json!({}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }),
            ConversationMessage::Tool(ToolMessage {
                id: id("message_2"),
                result: ToolResult {
                    call_id: id("call_1"),
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::Text("2026-07-17".to_owned()),
                },
            }),
        ]);
        let json = serde_json::to_string(&snapshot).expect("serialize conversation");
        assert_eq!(
            serde_json::from_str::<ConversationSnapshot>(&json).expect("deserialize conversation"),
            snapshot
        );
    }

    #[test]
    fn user_parts_distinguish_injected_text() {
        let turn = UserMessage {
            id: id("message_1"),
            parts: vec![
                UserPart::Injected(TextPart {
                    id: id("injected_1"),
                    text: "<constraint>answer briefly</constraint>".to_owned(),
                }),
                UserPart::Text(TextPart {
                    id: id("text_1"),
                    text: "What date is it?".to_owned(),
                }),
            ],
        };
        let json = serde_json::to_string(&turn).expect("serialize user turn");
        assert!(json.contains(r#""type":"injected""#));
        let decoded: UserMessage = serde_json::from_str(&json).expect("deserialize user turn");
        assert_eq!(decoded, turn);
        assert!(matches!(decoded.parts[0], UserPart::Injected(_)));
        assert!(matches!(decoded.parts[1], UserPart::Text(_)));
        // UI 展示时只保留用户真实输入，隐藏应用注入的片段。
        let visible: Vec<&TextPart> = decoded
            .parts
            .iter()
            .filter_map(|part| match part {
                UserPart::Text(text) => Some(text),
                UserPart::Injected(_) => None,
            })
            .collect();
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn provider_state_requires_explicit_boundaries() {
        let provider: ProviderId = id("openai");
        let protocol: ProtocolId = id("responses");
        assert!(
            OpaqueProviderState::new(
                provider.clone(),
                protocol.clone(),
                "",
                "application/json",
                1,
                vec![],
            )
            .is_err()
        );
        assert!(
            OpaqueProviderState::new(
                provider,
                protocol,
                "encrypted_reasoning",
                "application/json",
                0,
                vec![],
            )
            .is_err()
        );
    }
}
