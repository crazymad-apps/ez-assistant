use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// OpenAI Chat Completions 原生请求。
///
/// 字段名随 Profile 变化的参数（max tokens、reasoning effort）和匹配命名空间的
/// Provider 私有选项，都通过 `extra` 平铺合并进请求根。未知字段按 serde 默认行为忽略。
pub struct ChatRequest {
    /// Provider 侧模型名称。
    pub model: String,
    /// 按序排列的对话消息。
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具定义；无工具时整个字段省略。
    pub tools: Option<Vec<ChatTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具选择策略；无工具时一并省略。
    pub tool_choice: Option<ChatToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 采样温度；仅在 Profile 声明支持且请求设置时写入。
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// nucleus 采样阈值；仅在 Profile 声明支持且请求设置时写入。
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 有序 stop 序列；仅在 Profile 声明支持且非空时写入。
    pub stop: Option<Vec<String>>,
    /// 固定为 `true`；Adapter 只走流式调用。
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 流式选项；固定要求 Provider 在流末尾下发 usage。
    pub stream_options: Option<ChatStreamOptions>,
    #[serde(flatten)]
    /// 字段名随 Profile 变化的参数与 Provider 私有选项，原样写入请求根。
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 流式请求选项。
pub struct ChatStreamOptions {
    /// 要求 Provider 在流末尾返回 token 用量。
    pub include_usage: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
/// 按 role 区分的原生对话消息。
pub enum ChatMessage {
    /// 系统指令。
    System(ChatSystemMessage),
    /// 用户输入。
    User(ChatUserMessage),
    /// 模型响应。
    Assistant(ChatAssistantMessage),
    /// 工具执行结果。
    Tool(ChatToolMessage),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// role 为 `system` 的原生消息。
pub struct ChatSystemMessage {
    /// 系统指令正文。
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// role 为 `user` 的原生消息。
pub struct ChatUserMessage {
    /// 用户正文；单片段为纯字符串，多片段为 text part 数组。
    pub content: ChatUserContent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
/// user 消息正文的两种线上形态。
pub enum ChatUserContent {
    /// 纯字符串正文。
    Text(String),
    /// 有序 text part 数组。
    Parts(Vec<ChatContentPart>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// user 消息正文数组中的一个 text part。
pub struct ChatContentPart {
    #[serde(rename = "type")]
    /// part 类型，固定为 `text`。
    pub kind: ChatContentPartKind,
    /// 文本内容。
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// user 正文 part 的类型标记。
pub enum ChatContentPartKind {
    /// 纯文本 part。
    Text,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// role 为 `assistant` 的原生消息；也用于非流式响应中的 message。
///
/// `reasoning_content` 等随 Profile 变化的 reasoning 字段由 `extra` 平铺承接，
/// 解码时再按 [`crate::Profile::reasoning_content_field`] 查取。
pub struct ChatAssistantMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 反序列化独立 message 时吞掉的 role 标记；编码时 role 由外层 [`ChatMessage`] 写入。
    pub role: Option<String>,
    #[serde(default)]
    /// 正文文本；无正文时编码为空字符串，解码允许缺省或 `null`。
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 模型发起的工具调用。
    pub tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(flatten)]
    /// 随 Profile 变化的 reasoning 字段等额外字段。
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// role 为 `tool` 的原生消息，回填一次工具执行结果。
pub struct ChatToolMessage {
    /// 对应工具调用的 ID，与 assistant 消息中的 tool call 严格配对。
    pub tool_call_id: String,
    /// 返回给模型的结果正文；Chat Completions 协议只接受字符串。
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 暴露给模型的工具定义。
pub struct ChatTool {
    #[serde(rename = "type")]
    /// 工具类型，固定为 `function`。
    pub kind: ChatToolKind,
    /// 函数定义。
    pub function: ChatFunctionDefinition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 工具定义与工具调用的类型标记。
pub enum ChatToolKind {
    /// JSON Schema 描述的函数。
    Function,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 函数的线上定义。
pub struct ChatFunctionDefinition {
    /// 函数名称。
    pub name: String,
    /// 面向模型的用途说明。
    pub description: String,
    /// 输入参数的 JSON Schema。
    pub parameters: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
/// 工具选择策略的线上形态：字符串模式或指定 function。
pub enum ChatToolChoice {
    /// `auto` / `none` / `required` 字符串模式。
    Mode(ChatToolChoiceMode),
    /// 强制调用指定工具。
    Named(ChatNamedToolChoice),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 字符串形态的工具选择模式。
pub enum ChatToolChoiceMode {
    /// 由模型自行决定。
    Auto,
    /// 禁止使用工具。
    None,
    /// 至少调用一个工具。
    Required,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 强制调用指定工具的选择形态。
pub struct ChatNamedToolChoice {
    #[serde(rename = "type")]
    /// 类型标记，固定为 `function`。
    pub kind: ChatToolKind,
    /// 被强制的函数。
    pub function: ChatNamedToolChoiceFunction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 指定工具选择中的函数引用。
pub struct ChatNamedToolChoiceFunction {
    /// 函数名称。
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// assistant 消息中一次完整的工具调用。
pub struct ChatToolCall {
    /// Provider 分配的调用 ID。
    pub id: String,
    #[serde(rename = "type")]
    /// 类型标记，固定为 `function`。
    pub kind: ChatToolKind,
    /// 函数调用内容。
    pub function: ChatToolCallFunction,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 完整工具调用中的函数部分。
pub struct ChatToolCallFunction {
    /// 函数名称。
    pub name: String,
    /// 已 JSON 序列化的参数字符串（注意不是 JSON 值）。
    pub arguments: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
/// token 用量。
pub struct ChatUsage {
    /// 输入 token 数。
    pub prompt_tokens: u64,
    /// 输出 token 数。
    pub completion_tokens: u64,
    /// 总 token 数。
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 输入 token 明细。
    pub prompt_tokens_details: Option<ChatPromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 输出 token 明细。
    pub completion_tokens_details: Option<ChatCompletionTokensDetails>,
    #[serde(flatten)]
    /// Provider 私有的扁平 usage 字段（如 DeepSeek 的 `prompt_cache_hit_tokens` /
    /// `prompt_cache_miss_tokens`），解码时按 [`crate::Profile::cached_input_tokens_field`]
    /// 声明的字段名查取，其余字段保留但不消费。
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
/// 输入 token 明细。
pub struct ChatPromptTokensDetails {
    #[serde(default)]
    /// 命中缓存的输入 token 数。
    pub cached_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
/// 输出 token 明细。
pub struct ChatCompletionTokensDetails {
    #[serde(default)]
    /// 输出中 reasoning token 的数量。
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 非流式完整响应。
pub struct ChatResponse {
    /// 响应 ID。
    pub id: String,
    /// 生成响应的模型名称。
    pub model: String,
    /// 响应选择；本 Codec 只消费第一个。
    pub choices: Vec<ChatResponseChoice>,
    #[serde(default)]
    /// token 用量。
    pub usage: Option<ChatUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 非流式响应中的一个选择。
pub struct ChatResponseChoice {
    #[serde(default)]
    /// 选择序号。
    pub index: u32,
    /// 完整的 assistant 消息。
    pub message: ChatAssistantMessage,
    #[serde(default)]
    /// Provider 结束本次输出的原因。
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 流式响应的一个 chunk。
pub struct ChatChunk {
    /// chunk 所属响应的 ID。
    pub id: String,
    /// 生成响应的模型名称。
    pub model: String,
    /// chunk 选择；本 Codec 只消费第一个。
    pub choices: Vec<ChatChunkChoice>,
    #[serde(default)]
    /// token 用量；通常在流末尾的独立 chunk 中下发。
    pub usage: Option<ChatUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// 流式 chunk 中的一个选择。
pub struct ChatChunkChoice {
    #[serde(default)]
    /// 选择序号。
    pub index: u32,
    /// 本 chunk 的增量内容。
    pub delta: ChatChunkDelta,
    #[serde(default)]
    /// 非空表示 Provider 已结束本次输出。
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// 流式 chunk 的增量内容。
///
/// `reasoning_content` 等随 Profile 变化的 reasoning 字段由 `extra` 平铺承接。
pub struct ChatChunkDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 首个 chunk 携带的角色声明；Codec 不消费，仅用来避免落入 `extra`。
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 正文文本增量。
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 工具调用增量。
    pub tool_calls: Option<Vec<ChatToolCallDelta>>,
    #[serde(flatten)]
    /// 随 Profile 变化的 reasoning 字段等额外字段。
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// 一次工具调用的流式增量。
pub struct ChatToolCallDelta {
    /// 工具调用序号；同一 index 的增量聚合成一次调用。
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 调用 ID；通常只在首个增量中下发。
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 函数部分增量。
    pub function: Option<ChatToolCallFunctionDelta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
/// 工具调用增量中的函数部分。
pub struct ChatToolCallFunctionDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 函数名称；通常只在首个增量中下发。
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// 参数 JSON 文本片段，需要跨 chunk 拼接。
    pub arguments: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Provider 错误响应正文。
pub struct ChatErrorBody {
    /// 错误详情。
    pub error: ChatErrorDetail,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Provider 错误详情。
pub struct ChatErrorDetail {
    /// 已脱敏的 Provider 诊断信息。
    pub message: String,
    #[serde(default, rename = "type")]
    /// Provider 的错误类型字符串。
    pub kind: Option<String>,
    #[serde(default)]
    /// Provider 的错误代码字符串。
    pub code: Option<String>,
}
