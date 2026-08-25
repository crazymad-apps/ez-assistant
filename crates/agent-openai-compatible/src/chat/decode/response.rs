//! 非流式 Assistant 消息、完整响应与 Provider 错误正文解码。

use agent_model::ModelError;
use agent_types::{
    AssistantMessage, AssistantPart, MessageId, ModelIdentity, PartId, ReasoningPart, TextPart,
    ToolCall, ToolCallId, ToolName,
};
use serde_json::Value;

use super::super::{
    ChatProtocolAdapter,
    schema::{ChatAssistantMessage, ChatErrorBody, ChatResponse},
};

use super::{map_finish_reason, map_usage, reasoning_field_text};

/// 用于非流式响应解码与编码产物的回读：reasoning 字段（按 ChatProtocolAdapter 字段名查取）映射为
/// [`ReasoningPart`]，content 映射为 [`TextPart`]，tool_calls 映射为 [`ToolCall`] 并解析
/// arguments。part ID 按 `part_1`、`part_2` 顺序生成。
pub fn decode_assistant_message(
    message: &ChatAssistantMessage,
    adapter: &ChatProtocolAdapter,
) -> Result<Vec<AssistantPart>, ModelError> {
    let mut parts: Vec<AssistantPart> = Vec::new();
    let mut part_seq = 0;
    let mut next_part_id = || {
        part_seq += 1;
        PartId::new(format!("part_{part_seq}")).expect("generated part ids are never empty")
    };

    if let Some(text) = reasoning_field_text(
        &message.extra,
        &adapter.reasoning_response_fields,
        "assistant message",
    )? && !text.is_empty()
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
    adapter: &ChatProtocolAdapter,
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
        model: ModelIdentity::new(adapter.provider.clone(), response.model.clone()),
        parts: decode_assistant_message(&choice.message, adapter)?,
        finish_reason: map_finish_reason(finish_reason),
        usage: response
            .usage
            .as_ref()
            .map(|usage| map_usage(usage, adapter)),
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
            | "throttling_error" => {
                return ModelError::RateLimited {
                    message,
                    retry_after_ms: None,
                };
            }
            _ => {}
        }
    }
    ModelError::Provider {
        message,
        status: None,
    }
}
