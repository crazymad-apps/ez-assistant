mod validation;

pub use validation::ToolExchangeValidator;

use std::{borrow::Cow, collections::HashSet};

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use thiserror::Error;

use crate::{
    FinishReason, MessageId, ModelIdentity, PartId, ProtocolId, ProviderId, TokenUsage, ToolCallId,
    ToolName, ToolResult, insertion::InternalContextPart,
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
    /// 从后向前选取最新文本摘要或原生边界，保留系统前缀和边界后的规范事实。
    ///
    /// 压缩窗口属于调用方提供的可靠对话，不因模型、入口地址或临时续接绑定变化而失效。
    /// Provider/Protocol 与窗口格式由 Adapter 校验；响应 ID 的续接资格另行判断。
    ///
    /// # Errors
    /// 完整输入或裁剪结果破坏工具配对时返回结构错误。
    pub fn project_provider_context(&self) -> Result<Self, ConversationValidationError> {
        self.validate_tool_exchange_pairs()?;
        let prefix = self
            .messages
            .iter()
            .take_while(|message| matches!(message, ConversationMessage::System(_)))
            .count();
        let boundary = self.messages.iter().enumerate().rev().find_map(|(index, message)| {
            if matches!(message, ConversationMessage::ContextSummary(_)) {
                return Some((index, None));
            }
            let ConversationMessage::Assistant(assistant) = message else { return None; };
            assistant.parts.iter().rposition(|part| matches!(part, AssistantPart::ProviderState(state)
                if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext)
            )).map(|part| (index, Some(part)))
        });
        let mut messages = self.messages[..prefix].to_vec();
        if let Some((index, part)) = boundary {
            match (&self.messages[index], part) {
                (ConversationMessage::Assistant(assistant), Some(part)) => {
                    let mut assistant = assistant.clone();
                    assistant.parts.drain(..part);
                    messages.push(ConversationMessage::Assistant(assistant));
                }
                (ConversationMessage::ContextSummary(summary), None) => {
                    messages.push(self.messages[index].clone());
                    if let Some(reference) = &summary.retained_user_message_id {
                        let users: Vec<_> = self.messages.iter().enumerate().filter(|(_, message)| {
                            matches!(message, ConversationMessage::User(user) if &user.id == reference)
                        }).collect();
                        match users.as_slice() {
                            [(user_index, user)] if *user_index < index => {
                                messages.push((*user).clone())
                            }
                            // 已投影快照直接复用紧随摘要的原始 User，保持多层投影幂等。
                            [(user_index, _)] if *user_index == index + 1 => {}
                            _ => {
                                return Err(
                                    ConversationValidationError::InvalidProviderContextBoundary,
                                );
                            }
                        }
                    }
                }
                _ => unreachable!(),
            }
            // 最近边界替代之前的窗口；后续旧压缩记录不重复装配。
            messages.extend(
                self.messages[index + 1..]
                    .iter()
                    .filter(|message| {
                        !matches!(
                            message,
                            ConversationMessage::ContextSummary(_)
                                | ConversationMessage::Assistant(AssistantMessage {
                                    purpose: AssistantMessagePurpose::ContextCompaction,
                                    ..
                                })
                        )
                    })
                    .cloned(),
            );
        } else {
            messages.extend(
                self.messages[prefix..]
                    .iter()
                    .filter(|message| {
                        !matches!(
                            message,
                            ConversationMessage::ContextSummary(_)
                                | ConversationMessage::Assistant(AssistantMessage {
                                    purpose: AssistantMessagePurpose::ContextCompaction,
                                    ..
                                })
                        )
                    })
                    .cloned(),
            );
        }
        let projected = Self::new(messages);
        projected.validate_tool_exchange_pairs()?;
        Ok(projected)
    }

    /// 从有序对话消息创建快照。
    pub fn new(messages: Vec<ConversationMessage>) -> Self {
        Self { messages }
    }

    /// 严格校验快照内所有 Tool Call 与 Tool Result 的双向配对和顺序。
    ///
    /// 每个 ToolCallId 在同一压缩边界内必须唯一；含 Tool Call 的 AssistantMessage 后必须立即按
    /// 声明顺序出现恰好一个对应 ToolMessage，才能开始下一条非 Tool 消息。
    pub fn validate_tool_exchange_pairs(&self) -> Result<(), ConversationValidationError> {
        let mut validation = ToolExchangeValidator::default();
        for message in &self.messages {
            validation.push(message)?;
        }
        validation.finish()
    }

    /// 在接纳响应和执行工具前，核对本次请求快照与响应中的调用 ID。
    ///
    /// 只消费调用方实际发送的快照；不读取持久化历史、不修改 ID，也不要求响应已有工具结果。
    ///
    /// # Errors
    /// 响应内部重复，或响应与请求快照内任一工具调用重复时返回错误。
    pub fn validate_response_tool_call_ids(
        &self,
        response: &AssistantMessage,
    ) -> Result<(), ConversationValidationError> {
        self.validate_tool_exchange_pairs()?;
        let mut seen = HashSet::new();
        for message in &self.messages {
            if matches!(message, ConversationMessage::ContextSummary(_)) {
                seen.clear();
            }
            if let ConversationMessage::Assistant(assistant) = message {
                for part in &assistant.parts {
                    if matches!(part, AssistantPart::ProviderState(state) if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext))
                    {
                        seen.clear();
                    }
                    if let AssistantPart::ToolCall(call) = part {
                        seen.insert(&call.id);
                    }
                }
            }
        }
        let mut response_calls_seen = false;
        for part in &response.parts {
            if matches!(part, AssistantPart::ProviderState(state) if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext))
            {
                if response_calls_seen {
                    return Err(ConversationValidationError::InvalidProviderContextBoundary);
                }
                seen.clear();
            }
            if matches!(part, AssistantPart::ToolCall(_)) {
                response_calls_seen = true;
            }
            if let AssistantPart::ToolCall(call) = part
                && !seen.insert(&call.id)
            {
                return Err(ConversationValidationError::DuplicateToolCallId {
                    call_id: call.id.clone(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// 规范对话中的 Tool Call/Result 结构不满足双向配对约束。
pub enum ConversationValidationError {
    #[error("provider context boundary interrupts a tool exchange")]
    InvalidProviderContextBoundary,
    /// 同一个 ToolCallId 被声明多次。
    #[error("tool call id `{call_id}` is declared more than once")]
    DuplicateToolCallId {
        /// 重复的 ToolCallId。
        call_id: ToolCallId,
    },
    /// ToolMessage 没有对应当前 exchange 中的 Tool Call。
    #[error("tool result references call id `{call_id}` without a matching pending tool call")]
    ToolResultWithoutCall {
        /// 无法配对的 ToolCallId。
        call_id: ToolCallId,
    },
    /// Tool Result 没有按照 Tool Call 的声明顺序出现。
    #[error("tool results are out of order: expected `{expected}`, got `{actual}`")]
    ToolResultOutOfOrder {
        /// 当前应当结算的 ToolCallId。
        expected: ToolCallId,
        /// 实际到达的 ToolCallId。
        actual: ToolCallId,
    },
    /// Tool Call 在 exchange 结束前缺少结果。
    #[error("tool call id `{call_id}` has no matching tool result")]
    MissingToolResult {
        /// 缺少结果的 ToolCallId。
        call_id: ToolCallId,
    },
    /// 同一个 Tool Call 收到了多个结果。
    #[error("tool call id `{call_id}` has more than one tool result")]
    DuplicateToolResult {
        /// 重复结算的 ToolCallId。
        call_id: ToolCallId,
    },
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
    /// 由较早对话派生的上下文摘要。
    ContextSummary(ContextSummaryMessage),
    /// 用户输入。
    User(UserMessage),
    /// 模型生成的完整响应。
    Assistant(AssistantMessage),
    /// 对某个 Tool Call 的执行结果。
    Tool(ToolMessage),
}

impl ConversationMessage {
    /// 返回该消息是否属于产品中可见的用户/助手转录。
    ///
    /// System、Context Summary 和 Tool 始终不是产品转录项；Runtime 隐藏
    /// UserMessage 仍保留在规范上下文中，但不计入转录。
    pub fn is_transcript_visible(&self) -> bool {
        match self {
            Self::User(message) => message.transcript_visibility.is_visible(),
            Self::Assistant(message) => message.purpose.is_model_response(),
            Self::System(_) | Self::ContextSummary(_) | Self::Tool(_) => false,
        }
    }
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
/// 由较早规范对话派生、供后续模型请求使用的上下文摘要。
pub struct ContextSummaryMessage {
    /// 轮内回收时保留的原始用户消息；引用由宿主从完整历史解析，不复制写入正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_user_message_id: Option<MessageId>,
    /// 规范消息 ID。
    pub id: MessageId,
    /// 摘要正文。
    pub text: String,
    /// 生成摘要的模型；旧正文没有该字段时保持为空以兼容读取。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::ModelIdentity>,
    /// 摘要模型调用的实际用量；它属于本次模型调用，但不参与窗口预检。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::TokenUsage>,
    /// 被本摘要替换掉的历史模型调用累计用量；供 UI 恢复总量，不参与窗口预检。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_usage: Option<crate::TokenUsage>,
    /// 当前摘要替换历史后，窗口预检应如何近似修正 Provider usage。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_adjustment: Option<ContextUsageAdjustment>,
    /// 由程序维护的结构化上下文；存在时在摘要正文后以稳定格式呈现给模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub programmatic_context: Option<String>,
}

impl ContextSummaryMessage {
    /// 返回 Provider 应看到的统一摘要正文；旧摘要不产生任何 wire 变化。
    pub fn model_visible_text(&self) -> Cow<'_, str> {
        let Some(programmatic_context) = self.programmatic_context.as_deref() else {
            return Cow::Borrowed(&self.text);
        };
        Cow::Owned(format!(
            "{}\n\nProgrammatically maintained context. This section is authoritative when it conflicts with prose:\n{}",
            self.text, programmatic_context
        ))
    }
}

/// 摘要替换历史后用于窗口预检的近似 usage 修正。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextUsageAdjustment {
    /// 从仍保留的 Assistant usage 中减去压缩边界前最后一次已计量 usage。
    Subtract {
        /// replacement 中作为当前 usage 基准的 Assistant 消息。
        retained_assistant_id: MessageId,
        /// 被摘要替换掉的历史在该 usage 中已累计的近似 token 数。
        subtract_total_tokens: u64,
    },
    /// 无法安全建立减法边界；窗口预检保持 usage 不可用并交给 Provider 兜底。
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一次用户输入。
pub struct UserMessage {
    /// 规范消息 ID。
    pub id: MessageId,
    /// 谁创建了该 user role 消息；旧数据缺省为真实用户。
    #[serde(default)]
    pub origin: UserMessageOrigin,
    /// 该整条消息是否进入产品转录；不影响模型上下文可见性。
    #[serde(default)]
    pub transcript_visibility: TranscriptVisibility,
    /// 用户消息的有序内容片段，包含正文、应用注入文本和可见文件引用。
    pub parts: Vec<UserPart>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// user role 消息的业务创建者；不会编码到 Provider wire。
pub enum UserMessageOrigin {
    /// 由用户实际提交。
    #[default]
    User,
    /// 由 Runtime 为受控执行链内部创建。
    Runtime,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// UserMessage 在产品转录中的可见性；不会编码到 Provider wire。
pub enum TranscriptVisibility {
    /// 参与分页、搜索、导出和用户输入统计。
    #[default]
    Visible,
    /// 仅保留于规范上下文、回放和审计，不进入产品转录。
    Hidden,
}

impl TranscriptVisibility {
    /// 是否应进入产品转录。
    pub const fn is_visible(self) -> bool {
        matches!(self, Self::Visible)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
/// UserMessage 中可能出现的内容片段。
pub enum UserPart {
    /// 用户真实输入的文本。
    Text(TextPart),
    /// 上层应用注入的约束/上下文文本；UI 展示时隐藏，回放时仍进入发给模型的内容。
    Injected(TextPart),
    /// 结构化的 Runtime/上下文内部指令；正文进入模型，但不作为用户正文展示。
    InternalContext(InternalContextPart),
    /// 用户从既有会话正文中选取并冻结的结构化引用。
    QuotedText(QuotedTextPart),
    /// 用户可见的附件引用；不包含文件正文或应用层 Attachment ID。
    FileReferences(FileReferencesPart),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 引用来源在规范会话中的角色；不包含可用于绕过授权的消息身份。
pub enum QuotedTextSourceRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// 引用来源的应用内 Conversation owner；只用于宿主恢复来源，不进入 Provider wire。
pub enum QuotedTextSourceOwner {
    MainSession {
        session_id: String,
    },
    ChildTask {
        session_id: String,
        child_task_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一段随 UserMessage 持久化的冻结会话引用。
///
/// `exact` 是用户实际选中的可见正文；`prefix`/`suffix` 只用于有限上下文和预览。
/// 来源 locator 仅供宿主导航，不能作为授权凭据。
pub struct QuotedTextPart {
    /// 当前 UserMessage 内该引用的稳定 Part 身份；不会投影到 Provider wire。
    pub quote_id: PartId,
    pub exact: String,
    pub prefix: String,
    pub suffix: String,
    pub source_owner: QuotedTextSourceOwner,
    pub source_generation: u64,
    pub source_message_id: MessageId,
    pub text_start_utf16: u32,
    pub text_end_utf16: u32,
    pub source_role: QuotedTextSourceRole,
    pub source_label: String,
    /// 旧来源缺少可靠时间时保持为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_created_at_ms: Option<i64>,
    /// Runtime 在接受输入时核对来源 locator 后写入；失效不影响冻结正文。
    #[serde(default)]
    pub source_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一组按用户选择顺序保存的文件引用。
pub struct FileReferencesPart {
    /// 片段 ID，用于持久化回放与 UI 投影。
    pub id: PartId,
    /// Agent 可见的文件名称和稳定可读路径。
    pub files: Vec<FileReference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Agent 按需读取文件所需的最小稳定信息。
pub struct FileReference {
    /// 上传时保存的原始文件名。
    pub original_name: String,
    /// Runtime 分配并持久化的 Agent 可读路径。
    pub readable_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 一次完整的模型响应。
///
/// reasoning、正文和 Tool Call 都保存在同一个有序 `parts` 列表中，
/// 这样 DeepSeek 等 Provider 的下一轮请求可以恢复原始消息结构。
pub struct AssistantMessage {
    /// 状态压缩与普通回复分开记账和展示；缺省省略以保持旧消息哈希。
    #[serde(
        default,
        skip_serializing_if = "AssistantMessagePurpose::is_model_response"
    )]
    pub purpose: AssistantMessagePurpose,
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

/// 完整 Assistant 消息的来源用途；压缩状态不构成面向用户的普通回复。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessagePurpose {
    #[default]
    ModelResponse,
    ContextCompaction,
}

impl AssistantMessagePurpose {
    /// 缺省用途不写入旧规范消息，避免改变续接前缀。
    pub fn is_model_response(&self) -> bool {
        *self == Self::ModelResponse
    }
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

/// Provider 明确声明的可见 reasoning 来源；旧记录未标注时属于普通 reasoning。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSource {
    /// 普通或未标注摘要的内容，不表示模型全部内部推理。
    #[default]
    Raw,
    /// Provider 返回的摘要；宿主不生成或猜测摘要。
    Summary,
}

impl ReasoningSource {
    /// 省略缺省字段，保持旧规范消息的序列化及续接哈希稳定。
    pub fn is_raw(&self) -> bool {
        *self == Self::Raw
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一个 reasoning 内容片段。
pub struct ReasoningPart {
    /// 可见内容来源；旧 JSON 保持 raw，不能从片段 ID 推断。
    #[serde(default, skip_serializing_if = "ReasoningSource::is_raw")]
    pub source: ReasoningSource,
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
    #[error("provider context effect requires an exact route binding")]
    UnboundContextEffect,
    #[error("provider state type must not be empty")]
    EmptyStateType,
    #[error("provider state media type must not be empty")]
    EmptyMediaType,
    #[error("provider state format version must be greater than zero")]
    InvalidFormatVersion,
    #[error("provider state route binding must contain both related part and fingerprint")]
    IncompleteRouteBinding,
    #[error("provider state route fingerprint must be 64 lowercase hexadecimal characters")]
    InvalidRouteFingerprint,
    #[error("provider state payload exceeds the per-item byte limit")]
    PayloadTooLarge,
}

/// 单个普通不透明 Provider item 的最大原始 payload；完整替代窗口由传输预算约束。
pub const MAX_PROVIDER_STATE_ITEM_BYTES: usize = 2 * 1024 * 1024;
/// 一次普通 Assistant Turn 的不透明 Provider item 总量；不包含完整替代窗口。
pub const MAX_PROVIDER_STATE_TURN_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Eq, PartialEq, Serialize)]
/// 只能由对应 Provider Adapter 解释的不透明续传状态。
///
/// 普通 reasoning 文本不能放在这里；只有确实无法规范化、但下一次请求必须回传的
/// Provider 私有数据才使用这个类型。
pub struct OpaqueProviderState {
    #[serde(skip_serializing_if = "Option::is_none")]
    context_effect: Option<ProviderContextEffect>,
    provider: ProviderId,
    protocol: ProtocolId,
    state_type: String,
    media_type: String,
    format_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    related_part_id: Option<PartId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_fingerprint: Option<String>,
    payload: Vec<u8>,
}

/// 原生状态对其前文的中立语义；只描述可靠边界，不解释 Provider payload。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderContextEffect {
    ReplacePriorContext,
}

impl std::fmt::Debug for OpaqueProviderState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpaqueProviderState")
            .field("provider", &self.provider)
            .field("protocol", &self.protocol)
            .field("state_type", &self.state_type)
            .field("media_type", &self.media_type)
            .field("format_version", &self.format_version)
            .field("context_effect", &self.context_effect)
            .field("route_bound", &self.route_fingerprint.is_some())
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

// 这是 Serde 反序列化时使用的临时结构。它先接住 JSON 字段，随后必须调用
// `OpaqueProviderState::new` 做业务校验，防止外部 JSON 直接构造非法状态。
#[derive(Deserialize)]
struct OpaqueProviderStateWire {
    #[serde(default)]
    context_effect: Option<ProviderContextEffect>,
    provider: ProviderId,
    protocol: ProtocolId,
    state_type: String,
    media_type: String,
    format_version: u32,
    #[serde(default)]
    related_part_id: Option<PartId>,
    #[serde(default)]
    route_fingerprint: Option<String>,
    payload: Vec<u8>,
}

impl<'de> Deserialize<'de> for OpaqueProviderState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OpaqueProviderStateWire::deserialize(deserializer)?;
        Self::build(
            wire.provider,
            wire.protocol,
            wire.state_type,
            wire.media_type,
            wire.format_version,
            wire.related_part_id,
            wire.route_fingerprint,
            wire.payload,
            wire.context_effect,
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
        Self::build(
            provider,
            protocol,
            state_type,
            media_type,
            format_version,
            None,
            None,
            payload,
            None,
        )
    }

    /// 创建绑定精确模型路由的不透明状态；没有可见文本时不关联规范 Part。
    #[allow(clippy::too_many_arguments)]
    pub fn new_routed(
        provider: ProviderId,
        protocol: ProtocolId,
        state_type: impl Into<String>,
        media_type: impl Into<String>,
        format_version: u32,
        related_part_id: Option<PartId>,
        route_fingerprint: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<Self, ProviderStateError> {
        Self::build(
            provider,
            protocol,
            state_type,
            media_type,
            format_version,
            related_part_id,
            Some(route_fingerprint.into()),
            payload,
            None,
        )
    }

    /// 创建已经由 Adapter 校验的完整上下文替代窗口。
    ///
    /// 窗口可包含保留消息和图片，不使用普通原生 item 的字节上限；调用方负责传输预算。
    pub fn new_context_replacement(
        provider: ProviderId,
        protocol: ProtocolId,
        state_type: impl Into<String>,
        media_type: impl Into<String>,
        format_version: u32,
        route_fingerprint: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<Self, ProviderStateError> {
        Self::build(
            provider,
            protocol,
            state_type,
            media_type,
            format_version,
            None,
            Some(route_fingerprint.into()),
            payload,
            Some(ProviderContextEffect::ReplacePriorContext),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        provider: ProviderId,
        protocol: ProtocolId,
        state_type: impl Into<String>,
        media_type: impl Into<String>,
        format_version: u32,
        related_part_id: Option<PartId>,
        route_fingerprint: Option<String>,
        payload: Vec<u8>,
        context_effect: Option<ProviderContextEffect>,
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
        if related_part_id.is_some() && route_fingerprint.is_none() {
            return Err(ProviderStateError::IncompleteRouteBinding);
        }
        if route_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| !valid_route_fingerprint(fingerprint))
        {
            return Err(ProviderStateError::InvalidRouteFingerprint);
        }
        if context_effect.is_some() && route_fingerprint.is_none() {
            return Err(ProviderStateError::UnboundContextEffect);
        }
        if context_effect.is_none() && payload.len() > MAX_PROVIDER_STATE_ITEM_BYTES {
            return Err(ProviderStateError::PayloadTooLarge);
        }
        Ok(Self {
            context_effect,
            provider,
            protocol,
            state_type,
            media_type,
            format_version,
            related_part_id,
            route_fingerprint,
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

    /// 返回关联的规范 reasoning Part；纯加密状态和旧状态可能没有文本关联。
    pub fn related_part_id(&self) -> Option<&PartId> {
        self.related_part_id.as_ref()
    }

    /// 返回不含 credential 的精确路由指纹；普通 Debug 不展示其值。
    pub fn route_fingerprint(&self) -> Option<&str> {
        self.route_fingerprint.as_deref()
    }

    /// 以只读字节切片形式返回不透明 payload。
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// 返回显式声明的替代语义；旧状态始终为空。
    pub fn context_effect(&self) -> Option<ProviderContextEffect> {
        self.context_effect
    }

    /// 在 Adapter 校验完整原生窗口后标记边界；此方法不裁剪或解密 payload。
    ///
    /// # Errors
    /// 未绑定精确路由的旧状态不能声明替代前文。
    pub fn with_context_effect(
        mut self,
        effect: ProviderContextEffect,
    ) -> Result<Self, ProviderStateError> {
        if self.route_fingerprint.is_none() {
            return Err(ProviderStateError::UnboundContextEffect);
        }
        self.context_effect = Some(effect);
        Ok(self)
    }
}

fn valid_route_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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
    fn legacy_assistant_wire_stays_stable_and_compaction_purpose_is_hidden() {
        let legacy = r#"{"id":"legacy","model":{"provider":"deepseek","model":"deepseek-reasoner"},"parts":[],"finish_reason":{"type":"stop"},"usage":null}"#;
        let mut message: AssistantMessage = serde_json::from_str(legacy).expect("old message");
        assert!(message.purpose.is_model_response());
        assert_eq!(serde_json::to_string(&message).unwrap(), legacy);
        message.purpose = AssistantMessagePurpose::ContextCompaction;
        let wire = serde_json::to_string(&message).unwrap();
        let decoded: AssistantMessage = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded.purpose, AssistantMessagePurpose::ContextCompaction);
        assert!(!ConversationMessage::Assistant(decoded).is_transcript_visible());
    }

    #[test]
    fn routed_replacement_boundary_resets_only_closed_tool_exchanges() {
        let state = OpaqueProviderState::new_routed(
            id("deepseek"),
            id("openai.responses"),
            "responses.compacted_window",
            "application/json",
            1,
            None,
            "a".repeat(64),
            b"secret-window".to_vec(),
        )
        .unwrap()
        .with_context_effect(ProviderContextEffect::ReplacePriorContext)
        .unwrap();
        let mut boundary = tool_call_message("compact", &[]);
        if let ConversationMessage::Assistant(message) = &mut boundary {
            message.purpose = AssistantMessagePurpose::ContextCompaction;
            message
                .parts
                .push(AssistantPart::ProviderState(state.clone()));
        }
        let complete = ConversationSnapshot::new(vec![
            tool_call_message("before", &["call"]),
            tool_result_message("result-before", "call"),
            boundary.clone(),
            tool_call_message("after", &["call"]),
            tool_result_message("result-after", "call"),
        ]);
        complete
            .validate_tool_exchange_pairs()
            .expect("new scope after a closed boundary");
        let pending =
            ConversationSnapshot::new(vec![tool_call_message("before", &["call"]), boundary]);
        assert!(pending.validate_tool_exchange_pairs().is_err());
        let wire = serde_json::to_value(&state).unwrap();
        assert_eq!(
            serde_json::from_value::<OpaqueProviderState>(wire.clone()).unwrap(),
            state
        );
        let mut unbound = wire;
        unbound.as_object_mut().unwrap().remove("route_fingerprint");
        assert!(serde_json::from_value::<OpaqueProviderState>(unbound).is_err());
        let debug = format!("{state:?}");
        assert!(!debug.contains("secret-window"));
        assert!(!debug.contains(&"a".repeat(64)));
    }

    #[test]
    fn native_projection_uses_latest_text_or_native_boundary_and_expands_user_once() {
        let summary = |name: &str, reference| {
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                retained_user_message_id: reference,
                id: id(name),
                text: name.into(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: None,
                programmatic_context: None,
            })
        };
        let user = ConversationMessage::User(UserMessage {
            id: id("task"),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: vec![],
        });
        let state = OpaqueProviderState::new_routed(
            id("openai"),
            id("responses"),
            "responses.compacted_window",
            "application/json",
            1,
            None,
            "a".repeat(64),
            b"{}".to_vec(),
        )
        .unwrap()
        .with_context_effect(ProviderContextEffect::ReplacePriorContext)
        .unwrap();
        let mut native = tool_call_message("native-boundary", &[]);
        if let ConversationMessage::Assistant(assistant) = &mut native {
            assistant.purpose = AssistantMessagePurpose::ContextCompaction;
            assistant.parts.push(AssistantPart::ProviderState(state));
        }
        let text = summary("latest-text", Some(id("task")));
        let tail = tool_call_message("tail", &[]);
        let history = ConversationSnapshot::new(vec![
            user.clone(),
            native.clone(),
            text.clone(),
            tail.clone(),
        ]);
        let projected = history.project_provider_context().unwrap();
        assert_eq!(
            projected.messages,
            vec![text.clone(), user.clone(), tail.clone()]
        );
        assert_eq!(
            projected.project_provider_context().unwrap(),
            projected,
            "Core and Adapter projection must be idempotent"
        );
        let history = ConversationSnapshot::new(vec![
            user.clone(),
            text.clone(),
            tail.clone(),
            native.clone(),
        ]);
        assert_eq!(
            history.project_provider_context().unwrap().messages,
            vec![native]
        );
    }

    #[test]
    fn quoted_text_round_trips_without_losing_frozen_context() {
        let part = UserPart::QuotedText(QuotedTextPart {
            quote_id: id("quote-1"),
            exact: "small selection".to_owned(),
            prefix: "before <tag>".to_owned(),
            suffix: "after & more".to_owned(),
            source_owner: QuotedTextSourceOwner::ChildTask {
                session_id: "session-1".to_owned(),
                child_task_id: "child-1".to_owned(),
            },
            source_generation: 3,
            source_message_id: id("message-1"),
            text_start_utf16: 7,
            text_end_utf16: 22,
            source_role: QuotedTextSourceRole::Assistant,
            source_label: "Session A".to_owned(),
            source_created_at_ms: Some(123),
            source_available: true,
        });
        let json = serde_json::to_string(&part).expect("quote should serialize");
        assert_eq!(
            serde_json::from_str::<UserPart>(&json).expect("quote should deserialize"),
            part
        );
    }

    fn tool_call_message(message_id: &str, call_ids: &[&str]) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            purpose: crate::AssistantMessagePurpose::ModelResponse,
            id: id(message_id),
            model: ModelIdentity::new(id("deepseek"), "deepseek-reasoner"),
            parts: call_ids
                .iter()
                .map(|call_id| {
                    AssistantPart::ToolCall(ToolCall {
                        id: id(call_id),
                        name: ToolName::new("get_date").expect("valid tool name"),
                        arguments: serde_json::json!({}),
                    })
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        })
    }

    fn tool_result_message(message_id: &str, call_id: &str) -> ConversationMessage {
        ConversationMessage::Tool(ToolMessage {
            id: id(message_id),
            result: ToolResult {
                call_id: id(call_id),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("ok".to_owned()),
                metadata: None,
            },
        })
    }

    #[test]
    fn mixed_assistant_parts_preserve_order() {
        let turn = AssistantMessage {
            purpose: crate::AssistantMessagePurpose::ModelResponse,
            id: id("turn_1"),
            model: ModelIdentity::new(id("deepseek"), "deepseek-reasoner"),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    source: Default::default(),
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
                origin: Default::default(),
                transcript_visibility: Default::default(),
                id: id("message_1"),
                parts: vec![UserPart::Text(TextPart {
                    id: id("text_1"),
                    text: "What date is it?".to_owned(),
                })],
            }),
            ConversationMessage::Assistant(AssistantMessage {
                purpose: crate::AssistantMessagePurpose::ModelResponse,
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
                    content: ToolResultContent::text("2026-07-17".to_owned()),
                    metadata: None,
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
    fn context_summary_round_trips_with_an_explicit_role() {
        let message = ConversationMessage::ContextSummary(ContextSummaryMessage {
            retained_user_message_id: Some(id("original_user")),
            id: id("summary_1"),
            text: "The user selected the local-first architecture.".to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: None,
            programmatic_context: None,
        });
        let json = serde_json::to_string(&message).expect("serialize context summary");
        assert!(json.contains(r#""role":"context_summary""#));
        assert_eq!(
            serde_json::from_str::<ConversationMessage>(&json)
                .expect("deserialize context summary"),
            message
        );
    }

    #[test]
    fn tool_exchange_validation_accepts_complete_ordered_batches() {
        let snapshot = ConversationSnapshot::new(vec![
            tool_call_message("assistant_1", &["call_1", "call_2"]),
            tool_result_message("tool_1", "call_1"),
            tool_result_message("tool_2", "call_2"),
            tool_call_message("assistant_2", &["call_3"]),
            tool_result_message("tool_3", "call_3"),
        ]);

        assert_eq!(snapshot.validate_tool_exchange_pairs(), Ok(()));
    }

    #[test]
    fn tool_exchange_validation_rejects_duplicate_call_ids() {
        let snapshot = ConversationSnapshot::new(vec![
            tool_call_message("assistant_1", &["call_1"]),
            tool_result_message("tool_1", "call_1"),
            tool_call_message("assistant_2", &["call_1"]),
        ]);

        assert_eq!(
            snapshot.validate_tool_exchange_pairs(),
            Err(ConversationValidationError::DuplicateToolCallId {
                call_id: id("call_1"),
            })
        );
    }

    #[test]
    fn tool_call_scope_resets_at_summary_without_hiding_incomplete_exchanges() {
        let summary = ConversationMessage::ContextSummary(ContextSummaryMessage {
            retained_user_message_id: None,
            id: id("summary"),
            text: "compacted history".to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: None,
            programmatic_context: None,
        });
        let before = tool_call_message("before", &["call_1"]);
        let after = tool_call_message("after", &["call_1"]);
        let history = ConversationSnapshot::new(vec![
            before.clone(),
            tool_result_message("result_before", "call_1"),
            summary.clone(),
            after.clone(),
            tool_result_message("result_after", "call_1"),
        ]);
        assert_eq!(history.validate_tool_exchange_pairs(), Ok(()));
        assert!(matches!(
            ConversationSnapshot::new(vec![before, summary.clone()]).validate_tool_exchange_pairs(),
            Err(ConversationValidationError::MissingToolResult { .. })
        ));
        let ConversationMessage::Assistant(response) = after else {
            unreachable!()
        };
        let request = ConversationSnapshot::new(vec![summary]);
        assert_eq!(request.validate_response_tool_call_ids(&response), Ok(()));
        let ConversationMessage::Assistant(duplicate_batch) =
            tool_call_message("batch", &["call_1", "call_1"])
        else {
            unreachable!()
        };
        assert!(matches!(
            request.validate_response_tool_call_ids(&duplicate_batch),
            Err(ConversationValidationError::DuplicateToolCallId { .. })
        ));
    }

    #[test]
    fn legacy_context_summary_defaults_new_fields_and_keeps_wire_text() {
        let json = r#"{"role":"context_summary","turn":{"id":"summary_1","text":"legacy"}}"#;
        let message = serde_json::from_str::<ConversationMessage>(json).expect("legacy summary");
        let ConversationMessage::ContextSummary(summary) = message else {
            panic!("expected context summary");
        };
        assert_eq!(summary.usage_adjustment, None);
        assert_eq!(summary.programmatic_context, None);
        assert_eq!(summary.model_visible_text(), "legacy");
    }

    #[test]
    fn context_summary_renders_programmatic_context_after_prose() {
        let summary = ContextSummaryMessage {
            retained_user_message_id: None,
            id: id("summary_1"),
            text: "prose".to_owned(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: Some(ContextUsageAdjustment::Subtract {
                retained_assistant_id: id("assistant_1"),
                subtract_total_tokens: 12,
            }),
            programmatic_context: Some("{\"cwd\":\"/workspace\"}".to_owned()),
        };
        assert_eq!(
            summary.model_visible_text(),
            "prose\n\nProgrammatically maintained context. This section is authoritative when it conflicts with prose:\n{\"cwd\":\"/workspace\"}"
        );
        let json = serde_json::to_string(&summary).expect("serialize summary");
        assert!(json.contains(r#""type":"subtract""#));
        assert_eq!(
            serde_json::from_str::<ContextSummaryMessage>(&json).expect("deserialize summary"),
            summary
        );
    }

    #[test]
    fn tool_exchange_validation_rejects_missing_or_orphan_results() {
        let missing =
            ConversationSnapshot::new(vec![tool_call_message("assistant_1", &["call_1"])]);
        assert_eq!(
            missing.validate_tool_exchange_pairs(),
            Err(ConversationValidationError::MissingToolResult {
                call_id: id("call_1"),
            })
        );

        let orphan = ConversationSnapshot::new(vec![tool_result_message("tool_1", "call_1")]);
        assert_eq!(
            orphan.validate_tool_exchange_pairs(),
            Err(ConversationValidationError::ToolResultWithoutCall {
                call_id: id("call_1"),
            })
        );
    }

    #[test]
    fn incremental_tool_validation_keeps_pending_order_and_scope_between_batches() {
        let mut validation = ToolExchangeValidator::default();
        validation
            .push(&tool_call_message(
                "assistant-stream",
                &["call_1", "call_2"],
            ))
            .unwrap();
        assert!(!validation.is_complete());
        assert_eq!(
            validation.finish(),
            Err(ConversationValidationError::MissingToolResult {
                call_id: id("call_1")
            })
        );
        validation
            .push(&tool_result_message("first-result", "call_1"))
            .unwrap();
        assert!(!validation.is_complete());
        assert_eq!(
            validation.finish(),
            Err(ConversationValidationError::MissingToolResult {
                call_id: id("call_2")
            })
        );
        validation
            .push(&tool_result_message("second-result", "call_2"))
            .unwrap();
        assert!(validation.is_complete());
        assert_eq!(validation.finish(), Ok(()));
        assert_eq!(
            validation.push(&tool_call_message("later-batch", &["call_1"])),
            Err(ConversationValidationError::DuplicateToolCallId {
                call_id: id("call_1")
            })
        );
    }

    #[test]
    fn tool_exchange_validation_rejects_out_of_order_or_duplicate_results() {
        let out_of_order = ConversationSnapshot::new(vec![
            tool_call_message("assistant_1", &["call_1", "call_2"]),
            tool_result_message("tool_2", "call_2"),
        ]);
        assert_eq!(
            out_of_order.validate_tool_exchange_pairs(),
            Err(ConversationValidationError::ToolResultOutOfOrder {
                expected: id("call_1"),
                actual: id("call_2"),
            })
        );

        let duplicate = ConversationSnapshot::new(vec![
            tool_call_message("assistant_1", &["call_1"]),
            tool_result_message("tool_1", "call_1"),
            tool_result_message("tool_2", "call_1"),
        ]);
        assert_eq!(
            duplicate.validate_tool_exchange_pairs(),
            Err(ConversationValidationError::DuplicateToolResult {
                call_id: id("call_1"),
            })
        );
    }

    #[test]
    fn user_parts_distinguish_injected_text() {
        let turn = UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
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
                UserPart::Injected(_)
                | UserPart::InternalContext(_)
                | UserPart::QuotedText(_)
                | UserPart::FileReferences(_) => None,
            })
            .collect();
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn legacy_user_message_defaults_to_visible_user_origin() {
        let legacy = serde_json::json!({
            "id": "message_legacy",
            "parts": [{
                "type": "text",
                "data": {"id": "part_legacy", "text": "legacy"}
            }]
        });

        let decoded: UserMessage =
            serde_json::from_value(legacy).expect("deserialize legacy user message");
        assert_eq!(decoded.origin, UserMessageOrigin::User);
        assert_eq!(decoded.transcript_visibility, TranscriptVisibility::Visible);

        let encoded = serde_json::to_value(decoded).expect("serialize current user message");
        assert_eq!(encoded["origin"], "user");
        assert_eq!(encoded["transcript_visibility"], "visible");
    }

    #[test]
    fn internal_context_round_trips_without_changing_legacy_injected_shape() {
        let message = UserMessage {
            id: id("message_internal"),
            origin: UserMessageOrigin::Runtime,
            transcript_visibility: TranscriptVisibility::Hidden,
            parts: vec![UserPart::InternalContext(
                crate::InternalContextPart::new(
                    id("part_internal"),
                    "boundary_1",
                    "goal_continuation",
                    "continue",
                )
                .expect("internal context"),
            )],
        };
        let encoded = serde_json::to_value(&message).expect("serialize message");
        assert_eq!(encoded["parts"][0]["type"], "internal_context");
        assert_eq!(
            serde_json::from_value::<UserMessage>(encoded).expect("deserialize message"),
            message
        );

        let legacy = serde_json::json!({
            "id": "message_legacy_injected",
            "parts": [{
                "type": "injected",
                "data": {"id": "part_legacy_injected", "text": "legacy"}
            }]
        });
        assert!(matches!(
            serde_json::from_value::<UserMessage>(legacy)
                .expect("deserialize legacy injected")
                .parts
                .as_slice(),
            [UserPart::Injected(_)]
        ));

        let legacy_internal = serde_json::json!({
            "id": "message_legacy_internal",
            "origin": "runtime",
            "transcript_visibility": "hidden",
            "parts": [{
                "type": "internal_context",
                "data": {
                    "id": "part_legacy_internal",
                    "boundary_id": "boundary_legacy",
                    "kind": "goal_continuation",
                    "retention_key": "goal:legacy",
                    "text": "continue"
                }
            }]
        });
        let decoded = serde_json::from_value::<UserMessage>(legacy_internal)
            .expect("legacy retention key is ignored");
        let reencoded = serde_json::to_value(decoded).expect("serialize current internal context");
        assert!(reencoded["parts"][0]["data"].get("retention_key").is_none());

        let invalid = serde_json::json!({
            "id": "message_invalid_internal",
            "origin": "runtime",
            "transcript_visibility": "hidden",
            "parts": [{
                "type": "internal_context",
                "data": {
                    "id": "part_invalid_internal",
                    "boundary_id": "",
                    "kind": "goal_continuation",
                    "text": "continue"
                }
            }]
        });
        assert!(serde_json::from_value::<UserMessage>(invalid).is_err());
    }

    #[test]
    fn hidden_runtime_user_message_remains_a_context_message_not_a_transcript_item() {
        let hidden = ConversationMessage::User(UserMessage {
            origin: UserMessageOrigin::Runtime,
            transcript_visibility: TranscriptVisibility::Hidden,
            id: id("message_runtime"),
            parts: vec![UserPart::Injected(TextPart {
                id: id("part_runtime"),
                text: "<runtime>continue</runtime>".to_owned(),
            })],
        });
        let visible = ConversationMessage::User(UserMessage {
            origin: UserMessageOrigin::User,
            transcript_visibility: TranscriptVisibility::Visible,
            id: id("message_user"),
            parts: vec![UserPart::Text(TextPart {
                id: id("part_user"),
                text: "continue".to_owned(),
            })],
        });

        assert!(!hidden.is_transcript_visible());
        assert!(visible.is_transcript_visible());
        assert!(
            ConversationMessage::Assistant(AssistantMessage {
                purpose: crate::AssistantMessagePurpose::ModelResponse,
                id: id("message_assistant"),
                model: ModelIdentity::new(id("deepseek"), "deepseek-chat"),
                parts: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: None,
            })
            .is_transcript_visible()
        );
    }

    #[test]
    fn file_references_round_trip_in_user_part_order() {
        let turn = UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: id("message_1"),
            parts: vec![
                UserPart::Text(TextPart {
                    id: id("text_1"),
                    text: "Compare the files".to_owned(),
                }),
                UserPart::FileReferences(FileReferencesPart {
                    id: id("files_1"),
                    files: vec![
                        FileReference {
                            original_name: "a.pdf".to_owned(),
                            readable_path: "/stable/a.pdf".to_owned(),
                        },
                        FileReference {
                            original_name: "b.xlsx".to_owned(),
                            readable_path: "/stable/b.xlsx".to_owned(),
                        },
                    ],
                }),
            ],
        };

        let json = serde_json::to_string(&turn).expect("serialize user turn");
        assert!(json.contains(r#""type":"file_references""#));
        let decoded: UserMessage = serde_json::from_str(&json).expect("deserialize user turn");
        assert_eq!(decoded, turn);
        assert!(matches!(decoded.parts[0], UserPart::Text(_)));
        assert!(matches!(decoded.parts[1], UserPart::FileReferences(_)));
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

    #[test]
    fn routed_provider_state_is_bounded_redacted_and_backward_compatible() {
        let routed = OpaqueProviderState::new_routed(
            id("deepseek"),
            id("openai.responses"),
            "responses.reasoning_item",
            "application/json",
            1,
            Some(id("reasoning_1")),
            "a".repeat(64),
            br#"{"encrypted_content":"secret-marker"}"#.to_vec(),
        )
        .expect("routed state");
        assert_eq!(
            routed.related_part_id().map(PartId::as_str),
            Some("reasoning_1")
        );
        assert_eq!(routed.route_fingerprint(), Some("a".repeat(64).as_str()));
        let debug = format!("{routed:?}");
        assert!(debug.contains("route_bound: true"));
        assert!(debug.contains("payload_bytes"));
        assert!(!debug.contains("secret-marker"));
        assert!(!debug.contains(&"a".repeat(64)));

        let round_trip: OpaqueProviderState =
            serde_json::from_str(&serde_json::to_string(&routed).expect("serialize routed state"))
                .expect("deserialize routed state");
        assert_eq!(round_trip, routed);

        let legacy = serde_json::json!({
            "provider":"deepseek",
            "protocol":"openai.responses",
            "state_type":"legacy",
            "media_type":"application/json",
            "format_version":1,
            "payload":[1,2,3]
        });
        let legacy: OpaqueProviderState =
            serde_json::from_value(legacy).expect("legacy state without route binding");
        assert!(legacy.related_part_id().is_none());
        assert!(legacy.route_fingerprint().is_none());

        assert_eq!(
            OpaqueProviderState::new_routed(
                id("deepseek"),
                id("openai.responses"),
                "responses.reasoning_item",
                "application/json",
                1,
                Some(id("reasoning_1")),
                "not-a-fingerprint",
                Vec::new(),
            ),
            Err(ProviderStateError::InvalidRouteFingerprint)
        );
        assert_eq!(
            OpaqueProviderState::new(
                id("deepseek"),
                id("openai.responses"),
                "responses.reasoning_item",
                "application/json",
                1,
                vec![0; MAX_PROVIDER_STATE_ITEM_BYTES + 1],
            ),
            Err(ProviderStateError::PayloadTooLarge)
        );
    }
}

#[cfg(test)]
mod reasoning_source_tests {
    use super::*;

    #[test]
    fn legacy_reasoning_keeps_exact_bytes_and_summary_roundtrips() {
        // F06 hashes the canonical message: absence must remain absence after reading.
        let legacy = r#"{"id":"legacy-part","text":"legacy reasoning"}"#;
        let mut part: ReasoningPart = serde_json::from_str(legacy).unwrap();
        assert_eq!(part.source, ReasoningSource::Raw);
        assert_eq!(serde_json::to_string(&part).unwrap(), legacy);
        part.source = ReasoningSource::Summary;
        let encoded = serde_json::to_string(&part).unwrap();
        assert!(encoded.contains("\"source\":\"summary\""));
        assert_eq!(
            serde_json::from_str::<ReasoningPart>(&encoded).unwrap(),
            part
        );
    }
}
