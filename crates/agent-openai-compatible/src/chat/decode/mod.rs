//! OpenAI-compatible 响应解码入口与流式/非流式共享映射。

mod response;
mod stream;

pub use response::{decode_assistant_message, decode_error_body, decode_response};
pub use stream::ChunkAssembler;

use std::collections::BTreeMap;

use agent_model::ModelError;
use agent_types::{FinishReason, TokenUsage};
use serde_json::Value;

use super::{ChatProtocolAdapter, schema::ChatUsage};

/// 把原生 assistant 消息解码为规范 parts。
///
/// 按 ChatProtocolAdapter 的有序字段从 extra map 读取 reasoning 文本。
///
/// 显式 `null` 等价于字段缺席：DeepSeek 会在没有 reasoning 增量的 chunk 里下发
/// `"reasoning_content": null`。vLLM 当前字段 `reasoning` 优先于旧版
/// `reasoning_content`；只读取首个存在字段，避免兼容别名重复追加。其余非字符串值属于协议违反。
fn reasoning_field_text<'a>(
    extra: &'a BTreeMap<String, Value>,
    fields: &[String],
    context: &str,
) -> Result<Option<&'a str>, ModelError> {
    for field in fields {
        match extra.get(field) {
            None | Some(Value::Null) => {}
            Some(value) => {
                let text = value.as_str().ok_or_else(|| {
                    ModelError::Protocol(format!(
                        "reasoning field `{field}` in {context} is not a string"
                    ))
                })?;
                return Ok(Some(text));
            }
        }
    }
    Ok(None)
}

/// 把原生 finish reason 字符串映射为规范 [`FinishReason`]。
pub(super) fn map_finish_reason(raw: &str) -> FinishReason {
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
/// 嵌套明细缺失时再按 ChatProtocolAdapter 声明的扁平字段名查取（DeepSeek 的
/// `prompt_cache_hit_tokens`，见
/// <https://api-docs.deepseek.com/api/create-chat-completion/>）。
pub(super) fn map_usage(usage: &ChatUsage, adapter: &ChatProtocolAdapter) -> TokenUsage {
    let cached_input_tokens = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|details| details.cached_tokens)
        .or_else(|| {
            let field = adapter.cached_input_tokens_field.as_deref()?;
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
