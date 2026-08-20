use std::collections::BTreeMap;

use agent_model::{ReasoningEffort, ToolImageProjection};
use agent_types::{ProtocolId, ProviderId};
use serde_json::Value;

use crate::ToolSchemaDialect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 规范 reasoning Part 在历史请求中的回放范围。
pub enum ReasoningReplayPolicy {
    /// 不在线上历史中回放 reasoning。
    Drop,
    /// 只在同一 Assistant 消息包含 Tool Call 时回放 reasoning。
    ToolCallsOnly,
    /// 回放所有历史 reasoning。
    PreserveAll,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一个 OpenAI-compatible Provider 的协议方言配置。
///
/// Codec 不判断 Provider 名称；reasoning 字段名、generation 参数支持和
/// max tokens 字段名等差异都通过 ChatProtocolAdapter 显式表达，测试可以直接用字面量构造方言。
pub struct ChatProtocolAdapter {
    /// Provider 标识；同时用作 ProviderOptions 命名空间和响应模型的 provider。
    pub(crate) provider: ProviderId,
    /// Provider 使用的通信协议标识；本 crate 固定为 `openai.chat_completions`。
    pub(crate) protocol: ProtocolId,
    /// reasoning 文本在线上使用的字段名（如 `reasoning_content`）；`None` 表示不支持 reasoning。
    pub(crate) reasoning_content_field: Option<String>,
    /// reasoning 强度参数在线上使用的字段名；`None` 表示不支持 reasoning 强度配置。
    pub(crate) reasoning_effort_field: Option<String>,
    pub(crate) reasoning_effort_values: BTreeMap<ReasoningEffort, Value>,
    /// 是否支持 `temperature` 采样参数。
    pub(crate) supports_temperature: bool,
    /// 是否支持 `top_p` 采样参数。
    pub(crate) supports_top_p: bool,
    /// 是否支持 `stop` 序列参数。
    pub(crate) supports_stop: bool,
    /// 最大输出 token 参数在线上使用的字段名；`None` 表示不支持该参数。
    pub(crate) max_output_tokens_field: Option<String>,
    /// 是否接受显式的 `tool_choice` 参数。
    ///
    /// `ToolChoice::Auto` 不受影响：`auto` 是线上的默认值，编码器统一省略，
    /// 省略与显式传 `"auto"` 语义相同。本字段为 `false` 时，非 `Auto` 的显式
    /// 工具选择（`None` / `Required` / `Named`）在编码期返回 `ModelError::Config`，
    /// 而不是发出一个必然被 Provider 拒绝的请求。
    ///
    /// DeepSeek thinking 模式拒绝任何显式 `tool_choice`（400：
    /// "Thinking mode does not support this tool_choice"），官方 Tool Calls 示例
    /// 也不传该参数（<https://api-docs.deepseek.com/guides/thinking_mode/>），
    /// 因此 DeepSeek 方言为 `false`。
    pub(crate) supports_tool_choice: bool,
    /// Tool Result Image Part 的固定投影；Chat 首版只实现聚合 user input。
    pub(crate) tool_image_projection: ToolImageProjection,
    /// function parameters 使用的 Schema 方言。
    pub(crate) tool_schema_dialect: ToolSchemaDialect,
    /// 带 tool calls 的 assistant 消息是否必须回传 reasoning 字段。
    ///
    /// DeepSeek thinking 模式要求：发生工具调用的轮次，其 `reasoning_content` 必须在
    /// 后续所有请求中完整回传，否则 API 返回 400
    ///（<https://api-docs.deepseek.com/guides/thinking_mode/>）。Provider 偶尔会返回
    /// 不带 reasoning 的合法 tool call；为 `true` 时编码器在回放该轮次时
    /// 生成仅用于 wire 的占位字段，不伪造规范 reasoning part。
    pub(crate) reasoning_replay: ReasoningReplayPolicy,
    /// Provider usage 中扁平的缓存命中 token 字段名（如 DeepSeek 的
    /// `prompt_cache_hit_tokens`，见
    /// <https://api-docs.deepseek.com/api/create-chat-completion/>）；
    /// `None` 表示只识别 OpenAI 嵌套的 `prompt_tokens_details.cached_tokens`。
    pub(crate) cached_input_tokens_field: Option<String>,
}

impl ChatProtocolAdapter {
    /// 构造 OpenAI 基础方言：不支持 reasoning 字段，标准 generation 参数全部支持，
    /// max tokens 参数使用 `max_tokens` 字段名。
    ///
    /// 具体 Provider 的具名方言见 [`ChatProtocolAdapter::deepseek`]；调用方也可以用字面量
    /// 调整字段来构造自己的方言。
    pub fn openai_compatible(provider: ProviderId) -> Self {
        Self {
            provider,
            protocol: ProtocolId::new("openai.chat_completions")
                .expect("`openai.chat_completions` is a valid protocol id"),
            reasoning_content_field: None,
            reasoning_effort_field: None,
            reasoning_effort_values: BTreeMap::new(),
            supports_temperature: true,
            supports_top_p: true,
            supports_stop: true,
            max_output_tokens_field: Some("max_tokens".to_owned()),
            supports_tool_choice: true,
            tool_image_projection: ToolImageProjection::Unsupported,
            // OpenAI-compatible 只描述传输形态，不保证第三方服务完整实现
            // Draft 2020-12；默认采用 function calling 公共子集更稳妥。
            tool_schema_dialect: ToolSchemaDialect::OpenAiFunctionSubset,
            reasoning_replay: ReasoningReplayPolicy::Drop,
            cached_input_tokens_field: None,
        }
    }

    /// 构造 DeepSeek 具名方言（Chat Completions，thinking 模式）。
    ///
    /// 依据 DeepSeek 官方文档（2026-07 核对）：
    ///
    /// - thinking 开关是请求根部的 `thinking: {"type": "enabled"}` 对象，默认
    ///   `enabled`，适用 `deepseek-v4-flash` / `deepseek-v4-pro`
    ///   （<https://api-docs.deepseek.com/guides/thinking_mode/>）。开关不编码进
    ///   ChatProtocolAdapter：调用方经 `ModelRequest::provider_options` 的 `deepseek` 命名空间
    ///   传入，编码器按命名空间合并进请求根。
    ///   本 ChatProtocolAdapter 的响应校验始终按 thinking-enabled 语义执行；通过
    ///   `provider_options` 传入 `thinking.type = disabled` 不属于受支持形态。
    /// - thinking 模式下 `temperature` / `top_p` / `presence_penalty` /
    ///   `frequency_penalty` 不生效（设置不报错但无作用，见同一文档），因此
    ///   `supports_temperature` / `supports_top_p` 为 `false`；`stop` 与
    ///   `max_tokens` 未在不生效列表中，保持支持。
    /// - reasoning 文本线上字段为 `reasoning_content`；发生工具调用的 assistant
    ///   消息必须在后续请求中完整回传该字段，缺失时 API 返回 400，因此
    ///   `tool_calls_require_reasoning` 为 `true`（见同一文档 Tool Calls 一节）。
    /// - usage 的缓存统计是扁平的 `prompt_cache_hit_tokens` /
    ///   `prompt_cache_miss_tokens`，而非 OpenAI 嵌套明细
    ///   （<https://api-docs.deepseek.com/api/create-chat-completion/>）。
    /// - thinking 模式拒绝任何显式 `tool_choice`（400："Thinking mode does not
    ///   support this tool_choice"，官方 Tool Calls 示例也不传该参数，见同一
    ///   文档），因此 `supports_tool_choice` 为 `false`；`Auto` 由编码器统一
    ///   省略（与线上默认值等价），显式选择会在编码期返回 `ModelError::Config`。
    /// - 具名构造器保持不声明 effort；正式产品由静态模型目录编译具体模型的
    ///   `reasoning_effort` 映射，再通过 [`Self::with_reasoning`] 注入，避免把随模型
    ///   变化的能力固化在协议实现中。
    /// - 工具参数先降级为 function calling 兼容子集。领域层由 schemars 生成的
    ///   Draft 2020-12 Schema 会包含根级 `oneOf`、字符串 `const`、可空联合类型和
    ///   `uint` format；这些结构不是 DeepSeek 工具 Schema 的稳定公共子集。
    pub fn deepseek() -> Self {
        Self {
            provider: ProviderId::new("deepseek").expect("`deepseek` is a valid provider id"),
            protocol: ProtocolId::new("openai.chat_completions")
                .expect("`openai.chat_completions` is a valid protocol id"),
            reasoning_content_field: Some("reasoning_content".to_owned()),
            reasoning_effort_field: None,
            reasoning_effort_values: BTreeMap::new(),
            supports_temperature: false,
            supports_top_p: false,
            supports_stop: true,
            max_output_tokens_field: Some("max_tokens".to_owned()),
            supports_tool_choice: false,
            tool_image_projection: ToolImageProjection::Unsupported,
            tool_schema_dialect: ToolSchemaDialect::OpenAiFunctionSubset,
            reasoning_replay: ReasoningReplayPolicy::ToolCallsOnly,
            cached_input_tokens_field: Some("prompt_cache_hit_tokens".to_owned()),
        }
    }

    /// Adapter 是否声明可解码 reasoning 内容。
    pub fn supports_reasoning(&self) -> bool {
        self.reasoning_content_field.is_some()
    }

    pub fn with_reasoning(
        mut self,
        content_field: Option<&str>,
        effort_field: Option<&str>,
        effort_values: BTreeMap<ReasoningEffort, Value>,
    ) -> Self {
        self.reasoning_content_field = content_field.map(str::to_owned);
        self.reasoning_effort_field = effort_field.map(str::to_owned);
        self.reasoning_effort_values = effort_values;
        self
    }

    /// 设置规范 reasoning Part 在后续历史请求中的回放范围。
    ///
    /// 该策略属于具体 Provider 与模型批次的 wire 约束。调用方必须依据静态模型目录
    /// 命中的 Route 显式配置，不能从 OpenAI-compatible 协议形态推断。
    pub fn with_reasoning_replay(mut self, policy: ReasoningReplayPolicy) -> Self {
        self.reasoning_replay = policy;
        self
    }

    pub fn reasoning_replay_policy(&self) -> ReasoningReplayPolicy {
        self.reasoning_replay
    }

    pub fn with_tool_image_projection(mut self, projection: ToolImageProjection) -> Self {
        self.tool_image_projection = projection;
        self
    }
}
