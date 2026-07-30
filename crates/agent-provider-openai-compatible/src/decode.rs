use std::collections::{BTreeMap, HashMap};

use agent_model::{ModelError, ModelEvent};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ReasoningPart,
    TextPart, TokenUsage, ToolCall, ToolCallId, ToolName,
};
use serde_json::Value;

use crate::{
    Profile,
    schema::{
        ChatAssistantMessage, ChatChunk, ChatChunkDelta, ChatErrorBody, ChatResponse,
        ChatToolCallDelta, ChatUsage,
    },
};

/// 把流式 chunk 聚合为规范事件的有状态组装器。
///
/// 逐块喂入 [`ChatChunk`]，每次返回零到多个 [`ModelEvent`]；流结束后调用
/// [`ChunkAssembler::finalize`] 产出唯一的 `TurnFinished`。组装器只保证事件序列满足
/// 规范生命周期；严格的 Provider 协议违例（完成后的 tool index、重复 call id、
/// 缺失 finish_reason 等）以 [`ModelError::Protocol`] 显式失败。
pub struct ChunkAssembler {
    profile: Profile,
    /// 首个 chunk 确立的响应身份（消息 ID 与模型）。
    turn: Option<(MessageId, ModelIdentity)>,
    /// 首个 choice chunk 确立的 choice 序号；后续 choice chunk 必须一致。
    choice_index: Option<u32>,
    /// 当前开放的内容 part；任一时刻最多一个（reasoning 或 text）。
    open_part: Option<OpenPart>,
    /// 下一个 part 序号，用于生成 `part_1`、`part_2` 等 ID。
    part_seq: u32,
    /// 已完成的内容与工具调用 part，按 Provider 输出顺序排列。
    parts: Vec<AssistantPart>,
    /// 按 index 聚合的工具调用状态。
    tool_calls: BTreeMap<u32, ToolCallState>,
    /// 已出现的非空 call id 到 index 的映射，用于发现跨 index 重复。
    known_call_ids: HashMap<String, u32>,
    /// 已收到的 finish reason；非空后不再接受携带 choice 的 chunk。
    finish_reason: Option<FinishReason>,
    /// 最新的 token 用量快照，后者覆盖前者。
    usage: Option<TokenUsage>,
}

/// 开放中的内容 part。
enum OpenPart {
    /// reasoning part。
    Reasoning { id: PartId, text: String },
    /// 正文 text part。
    Text { id: PartId, text: String },
}

/// 单个 index 的工具调用聚合状态。
#[derive(Default)]
struct ToolCallState {
    /// Provider 下发的调用 ID 原文。
    id: Option<String>,
    /// Provider 下发的函数名原文。
    name: Option<String>,
    /// 已拼接的参数 JSON 文本。
    args: String,
    /// 校验后的规范调用 ID；`ToolCallStarted` 发出后有值。
    call_id: Option<ToolCallId>,
    /// 校验后的规范工具名；`ToolCallStarted` 发出后有值。
    tool_name: Option<ToolName>,
    /// 是否已发出 `ToolCallStarted`。
    started: bool,
    /// 是否已发出 `ToolCallFinished`。
    completed: bool,
}

impl ChunkAssembler {
    /// 创建按给定方言解码的组装器。
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            turn: None,
            choice_index: None,
            open_part: None,
            part_seq: 0,
            parts: Vec::new(),
            tool_calls: BTreeMap::new(),
            known_call_ids: HashMap::new(),
            finish_reason: None,
            usage: None,
        }
    }

    /// 喂入一个流式 chunk，返回本次产出的规范事件（可能为空）。
    pub fn push_chunk(&mut self, chunk: &ChatChunk) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = Vec::new();
        if self.turn.is_none() {
            let message_id = MessageId::new(chunk.id.clone()).map_err(|error| {
                ModelError::Protocol(format!("chunk id is not a valid message id: {error}"))
            })?;
            let model = ModelIdentity::new(self.profile.provider.clone(), chunk.model.clone());
            events.push(ModelEvent::TurnStarted {
                message_id: message_id.clone(),
                model: model.clone(),
            });
            self.turn = Some((message_id, model));
        }

        // 身份一致性：后续 chunk（含空 choices 的 usage chunk）必须来自首个
        // chunk 确立的响应；跨响应或跨模型的数据不得合并进同一条消息。
        {
            let (message_id, model) = self.turn.as_ref().expect("turn established above");
            if chunk.id != message_id.as_str() || chunk.model != model.model {
                return Err(ModelError::Protocol(format!(
                    "chunk identity changed mid-stream: expected response `{}` from model `{}`, got `{}` from `{}`",
                    message_id.as_str(),
                    model.model,
                    chunk.id,
                    chunk.model
                )));
            }
        }

        // 多 choice（n > 1）不在本协议范围内，只消费第一个 choice。
        let Some(choice) = chunk.choices.first() else {
            // 空 choices 的 chunk 只可能携带 usage（如 OpenAI 流末的独立 usage chunk）；
            // 即使出现在 finish_reason 之后也合法。
            if let Some(usage) = &chunk.usage {
                let usage = map_usage(usage, &self.profile);
                self.usage = Some(usage.clone());
                events.push(ModelEvent::UsageUpdated { usage });
            }
            return Ok(events);
        };

        // choice 序号一经确立不得变化；变化说明流里混入了另一个 choice 的数据。
        match self.choice_index {
            Some(index) if index != choice.index => {
                return Err(ModelError::Protocol(format!(
                    "chunk choice index changed mid-stream: expected {index}, got {}",
                    choice.index
                )));
            }
            None => self.choice_index = Some(choice.index),
            _ => {}
        }

        if self.finish_reason.is_some() {
            return Err(ModelError::Protocol(
                "received a choice chunk after finish_reason".to_owned(),
            ));
        }

        self.push_delta_events(&choice.delta, &mut events)?;

        if let Some(finish_reason) = &choice.finish_reason {
            self.finish_reason = Some(map_finish_reason(finish_reason));
            self.close_open_part(&mut events);
            self.complete_tool_calls(.., &mut events)?;
        }

        if let Some(usage) = &chunk.usage {
            let usage = map_usage(usage, &self.profile);
            self.usage = Some(usage.clone());
            events.push(ModelEvent::UsageUpdated { usage });
        }
        Ok(events)
    }

    /// 结束流并产出唯一的 `TurnFinished`。
    ///
    /// 严格语义：没见过任何 chunk、或流在没有 finish_reason 的情况下结束，都属于
    /// 协议违例（传输中断或无终态关闭），以 [`ModelError::Protocol`] 失败而不是
    /// 伪造一个正常终态。
    pub fn finalize(&mut self) -> Result<Vec<ModelEvent>, ModelError> {
        let Some((message_id, model)) = self.turn.clone() else {
            return Err(ModelError::Protocol(
                "stream ended before any chunk arrived".to_owned(),
            ));
        };
        let Some(finish_reason) = self.finish_reason.clone() else {
            return Err(ModelError::Protocol(
                "stream ended without finish_reason".to_owned(),
            ));
        };
        // 防御性补齐收尾；finish_reason 处理路径已经关闭过这些内容。
        let mut events = Vec::new();
        self.close_open_part(&mut events);
        self.complete_tool_calls(.., &mut events)?;
        let message = AssistantMessage {
            id: message_id,
            model,
            parts: std::mem::take(&mut self.parts),
            finish_reason,
            usage: self.usage.clone(),
        };
        events.push(ModelEvent::TurnFinished { message });
        Ok(events)
    }

    /// 处理一个 delta，产出内容 part 与工具调用事件。
    fn push_delta_events(
        &mut self,
        delta: &ChatChunkDelta,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelError> {
        if let Some(field) = &self.profile.reasoning_content_field
            && let Some(text) = reasoning_field_text(&delta.extra, field, "chunk delta")?
            && !text.is_empty()
        {
            self.push_reasoning_delta(text, events);
        }
        if let Some(content) = &delta.content
            && !content.is_empty()
        {
            self.push_text_delta(content, events);
        }
        if let Some(tool_calls) = &delta.tool_calls {
            for call in tool_calls {
                self.push_tool_call_delta(call, events)?;
            }
        }
        Ok(())
    }

    /// 处理 reasoning 文本增量，必要时先关闭当前 part 再开启新的 reasoning part。
    fn push_reasoning_delta(&mut self, text: &str, events: &mut Vec<ModelEvent>) {
        if !matches!(self.open_part, Some(OpenPart::Reasoning { .. })) {
            self.close_open_part(events);
            let id = self.next_part_id();
            events.push(ModelEvent::ReasoningStarted { id: id.clone() });
            self.open_part = Some(OpenPart::Reasoning {
                id,
                text: String::new(),
            });
        }
        if let Some(OpenPart::Reasoning { id, text: buffer }) = &mut self.open_part {
            buffer.push_str(text);
            events.push(ModelEvent::ReasoningDelta {
                id: id.clone(),
                delta: text.to_owned(),
            });
        }
    }

    /// 处理正文文本增量，必要时先关闭当前 part 再开启新的 text part。
    fn push_text_delta(&mut self, text: &str, events: &mut Vec<ModelEvent>) {
        if !matches!(self.open_part, Some(OpenPart::Text { .. })) {
            self.close_open_part(events);
            let id = self.next_part_id();
            events.push(ModelEvent::TextStarted { id: id.clone() });
            self.open_part = Some(OpenPart::Text {
                id,
                text: String::new(),
            });
        }
        if let Some(OpenPart::Text { id, text: buffer }) = &mut self.open_part {
            buffer.push_str(text);
            events.push(ModelEvent::TextDelta {
                id: id.clone(),
                delta: text.to_owned(),
            });
        }
    }

    /// 关闭当前开放的内容 part，并把完成的 part 追加到输出序列。
    fn close_open_part(&mut self, events: &mut Vec<ModelEvent>) {
        match self.open_part.take() {
            Some(OpenPart::Reasoning { id, text }) => {
                events.push(ModelEvent::ReasoningFinished { id: id.clone() });
                self.parts
                    .push(AssistantPart::Reasoning(ReasoningPart { id, text }));
            }
            Some(OpenPart::Text { id, text }) => {
                events.push(ModelEvent::TextFinished { id: id.clone() });
                self.parts.push(AssistantPart::Text(TextPart { id, text }));
            }
            None => {}
        }
    }

    /// 生成下一个 part ID（`part_1`、`part_2`……）。
    fn next_part_id(&mut self) -> PartId {
        self.part_seq += 1;
        let seq = self.part_seq;
        PartId::new(format!("part_{seq}")).expect("generated part ids are never empty")
    }

    /// 处理一个工具调用增量。
    ///
    /// Provider 按 index 流式输出工具调用；出现更大的新 index 意味着更小 index 的
    /// 调用已经输出完毕，先把它们收尾为 `ToolCallFinished`。id 与 name 齐备时发出
    /// `ToolCallStarted`，在此之前到达的 arguments 片段先缓冲、started 后按序补发。
    fn push_tool_call_delta(
        &mut self,
        call: &ChatToolCallDelta,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelError> {
        let index = call.index;
        self.complete_tool_calls(..index, events)?;

        let state = self.tool_calls.entry(index).or_default();
        if state.completed {
            return Err(ModelError::Protocol(format!(
                "received a tool call delta for completed index {index}"
            )));
        }

        if let Some(id) = &call.id
            && !id.is_empty()
        {
            if let Some(existing) = &state.id
                && existing != id
            {
                return Err(ModelError::Protocol(format!(
                    "tool call at index {index} changed id from `{existing}` to `{id}`"
                )));
            }
            if state.id.is_none() {
                if let Some(previous) = self.known_call_ids.get(id) {
                    return Err(ModelError::Protocol(format!(
                        "tool call id `{id}` appears at both index {previous} and index {index}"
                    )));
                }
                state.id = Some(id.clone());
                self.known_call_ids.insert(id.clone(), index);
            }
        }

        if let Some(function) = &call.function
            && let Some(name) = &function.name
            && !name.is_empty()
        {
            if let Some(existing) = &state.name
                && existing != name
            {
                return Err(ModelError::Protocol(format!(
                    "tool call at index {index} changed name from `{existing}` to `{name}`"
                )));
            }
            if state.name.is_none() {
                state.name = Some(name.clone());
            }
        }

        if !state.started
            && let (Some(id), Some(name)) = (&state.id, &state.name)
        {
            let call_id = ToolCallId::new(id.clone()).map_err(|error| {
                ModelError::Protocol(format!(
                    "tool call id `{id}` at index {index} is invalid: {error}"
                ))
            })?;
            let tool_name = ToolName::new(name.clone()).map_err(|error| {
                ModelError::Protocol(format!(
                    "tool name `{name}` at index {index} is invalid: {error}"
                ))
            })?;
            events.push(ModelEvent::ToolCallStarted {
                id: call_id.clone(),
                name: tool_name.clone(),
            });
            state.call_id = Some(call_id.clone());
            state.tool_name = Some(tool_name);
            state.started = true;
            // 补齐 started 之前缓冲的 arguments 片段，保持增量顺序。
            if !state.args.is_empty() {
                events.push(ModelEvent::ToolCallDelta {
                    id: call_id,
                    arguments_delta: state.args.clone(),
                });
            }
        }

        if let Some(function) = &call.function
            && let Some(arguments) = &function.arguments
            && !arguments.is_empty()
        {
            state.args.push_str(arguments);
            if state.started
                && let Some(call_id) = &state.call_id
            {
                events.push(ModelEvent::ToolCallDelta {
                    id: call_id.clone(),
                    arguments_delta: arguments.clone(),
                });
            }
        }
        Ok(())
    }

    /// 把给定范围内未完成的工具调用按 index 升序收尾。
    fn complete_tool_calls(
        &mut self,
        range: impl std::ops::RangeBounds<u32>,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelError> {
        let pending: Vec<u32> = self
            .tool_calls
            .range(range)
            .filter(|(_, state)| !state.completed)
            .map(|(index, _)| *index)
            .collect();
        for index in pending {
            self.complete_tool_call(index, events)?;
        }
        Ok(())
    }

    /// 收尾单个工具调用：解析完整 arguments 并发出 `ToolCallFinished`。
    fn complete_tool_call(
        &mut self,
        index: u32,
        events: &mut Vec<ModelEvent>,
    ) -> Result<(), ModelError> {
        let Some(state) = self.tool_calls.get_mut(&index) else {
            return Ok(());
        };
        if state.completed {
            return Ok(());
        }
        let (Some(call_id), Some(tool_name)) = (state.call_id.clone(), state.tool_name.clone())
        else {
            return Err(ModelError::Protocol(format!(
                "tool call at index {index} finished before its id and name arrived"
            )));
        };
        // 部分 Provider 对无参数工具不下发任何 arguments 片段，按空对象处理。
        let arguments = if state.args.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&state.args).map_err(|error| {
                ModelError::ToolArguments(format!(
                    "tool call at index {index} has malformed arguments JSON: {error}"
                ))
            })?
        };
        state.completed = true;
        events.push(ModelEvent::ToolCallFinished {
            id: call_id.clone(),
            arguments: arguments.clone(),
        });
        self.parts.push(AssistantPart::ToolCall(ToolCall {
            id: call_id,
            name: tool_name,
            arguments,
        }));
        Ok(())
    }
}

/// 把原生 assistant 消息解码为规范 parts。
///
/// 按 Profile 字段名从 extra map 读取 reasoning 文本。
///
/// 显式 `null` 等价于字段缺席：DeepSeek 会在没有 reasoning 增量的 chunk 里下发
/// `"reasoning_content": null`。其余非字符串值属于协议违反。
fn reasoning_field_text<'a>(
    extra: &'a BTreeMap<String, Value>,
    field: &str,
    context: &str,
) -> Result<Option<&'a str>, ModelError> {
    match extra.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                ModelError::Protocol(format!(
                    "reasoning field `{field}` in {context} is not a string"
                ))
            })?;
            Ok(Some(text))
        }
    }
}

/// 用于非流式响应解码与编码产物的回读：reasoning 字段（按 Profile 字段名查取）映射为
/// [`ReasoningPart`]，content 映射为 [`TextPart`]，tool_calls 映射为 [`ToolCall`] 并解析
/// arguments。part ID 按 `part_1`、`part_2` 顺序生成。
pub fn decode_assistant_message(
    message: &ChatAssistantMessage,
    profile: &Profile,
) -> Result<Vec<AssistantPart>, ModelError> {
    let mut parts: Vec<AssistantPart> = Vec::new();
    let mut part_seq = 0;
    let mut next_part_id = || {
        part_seq += 1;
        PartId::new(format!("part_{part_seq}")).expect("generated part ids are never empty")
    };

    if let Some(field) = &profile.reasoning_content_field
        && let Some(text) = reasoning_field_text(&message.extra, field, "assistant message")?
        && !text.is_empty()
    {
        parts.push(AssistantPart::Reasoning(ReasoningPart {
            id: next_part_id(),
            text: text.to_owned(),
        }));
    }
    if let Some(content) = &message.content
        && !content.is_empty()
    {
        parts.push(AssistantPart::Text(TextPart {
            id: next_part_id(),
            text: content.clone(),
        }));
    }
    if let Some(tool_calls) = &message.tool_calls {
        for call in tool_calls {
            let id = ToolCallId::new(call.id.clone()).map_err(|error| {
                ModelError::Protocol(format!(
                    "tool call id `{}` in assistant message is invalid: {error}",
                    call.id
                ))
            })?;
            let name = ToolName::new(call.function.name.clone()).map_err(|error| {
                ModelError::Protocol(format!(
                    "tool name `{}` in assistant message is invalid: {error}",
                    call.function.name
                ))
            })?;
            // 与流式组装一致：空 arguments 按空对象处理。
            let arguments = if call.function.arguments.is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ModelError::ToolArguments(format!(
                        "tool call `{}` has malformed arguments JSON: {error}",
                        call.id
                    ))
                })?
            };
            parts.push(AssistantPart::ToolCall(ToolCall {
                id,
                name,
                arguments,
            }));
        }
    }
    Ok(parts)
}

/// 把非流式完整响应解码为规范 [`AssistantMessage`]。
pub fn decode_response(
    response: &ChatResponse,
    profile: &Profile,
) -> Result<AssistantMessage, ModelError> {
    // 多 choice（n > 1）不在本协议范围内，只消费第一个 choice。
    let Some(choice) = response.choices.first() else {
        return Err(ModelError::Protocol(
            "response contains no choices".to_owned(),
        ));
    };
    let Some(finish_reason) = &choice.finish_reason else {
        return Err(ModelError::Protocol(
            "response choice is missing finish_reason".to_owned(),
        ));
    };
    let id = MessageId::new(response.id.clone()).map_err(|error| {
        ModelError::Protocol(format!("response id is not a valid message id: {error}"))
    })?;
    Ok(AssistantMessage {
        id,
        model: ModelIdentity::new(profile.provider.clone(), response.model.clone()),
        parts: decode_assistant_message(&choice.message, profile)?,
        finish_reason: map_finish_reason(finish_reason),
        usage: response
            .usage
            .as_ref()
            .map(|usage| map_usage(usage, profile)),
    })
}

/// 把 Provider 错误正文解析为规范错误。
///
/// 只做 type/code 字符串映射；HTTP 状态码分类属于 Transport 层，这里不处理。
pub fn decode_error_body(body: &ChatErrorBody) -> ModelError {
    let message = body.error.message.clone();
    let kinds = [body.error.code.as_deref(), body.error.kind.as_deref()];
    for kind in kinds.into_iter().flatten() {
        match kind {
            "context_length_exceeded" => {
                return ModelError::ContextOverflow { message };
            }
            "authentication_error"
            | "invalid_api_key"
            | "invalid_authentication"
            | "unauthorized" => return ModelError::Auth(message),
            "insufficient_quota"
            | "rate_limit_error"
            | "rate_limit_exceeded"
            | "throttling_error" => return ModelError::RateLimited(message),
            _ => {}
        }
    }
    ModelError::Provider {
        message,
        status: None,
    }
}

/// 把原生 finish reason 字符串映射为规范 [`FinishReason`]。
fn map_finish_reason(raw: &str) -> FinishReason {
    match raw {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_owned()),
    }
}

/// 把原生 usage 映射为规范 [`TokenUsage`]。
///
/// 缓存命中输入 token 优先取 OpenAI 嵌套的 `prompt_tokens_details.cached_tokens`；
/// 嵌套明细缺失时再按 Profile 声明的扁平字段名查取（DeepSeek 的
/// `prompt_cache_hit_tokens`，见
/// <https://api-docs.deepseek.com/api/create-chat-completion/>）。
fn map_usage(usage: &ChatUsage, profile: &Profile) -> TokenUsage {
    let cached_input_tokens = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .or_else(|| {
            let field = profile.cached_input_tokens_field.as_deref()?;
            usage.extra.get(field)?.as_u64()
        });
    TokenUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cached_input_tokens,
        reasoning_tokens: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    }
}
