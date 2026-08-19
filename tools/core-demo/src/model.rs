//! Core Demo 私有的离线确定性模型。
//!
//! 普通文本得到确定性回复；`/tool <name> <json>` 会产生一次真实工具调用，工具结果进入
//! Conversation 后，下一 Step 再返回最终文本。它只用于验证 SDK/Core 闭环。

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use agent_model::{
    ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelRequest,
    ModelService, ModelStreamFuture,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason, MessageId, ModelIdentity,
    PartId, ProviderId, ReasoningPart, TextPart, TokenUsage, ToolCall, ToolCallId, ToolName,
    UserPart,
};
use async_stream::stream;

pub(crate) struct DeterministicModel {
    capabilities: ModelCapabilities,
    next_message: AtomicU64,
    chunk_delay: Duration,
    context_window_tokens: u64,
}

impl Default for DeterministicModel {
    fn default() -> Self {
        Self::new(Duration::from_millis(20))
    }
}

impl DeterministicModel {
    pub(crate) fn new(chunk_delay: Duration) -> Self {
        Self::with_context_window(chunk_delay, 128_000)
    }

    pub(crate) fn with_context_window(chunk_delay: Duration, context_window_tokens: u64) -> Self {
        Self {
            capabilities: ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                streaming: true,
            },
            next_message: AtomicU64::new(0),
            chunk_delay,
            context_window_tokens,
        }
    }
}

impl ModelService for DeterministicModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
    fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let sequence = self.next_message.fetch_add(1, Ordering::Relaxed) + 1;
        let message_id = valid_message_id(&format!("demo-message-{sequence}"));
        let user_text = latest_user_text(&request);
        let response = response_for(&request, sequence, &user_text);
        let cancellation = context.cancellation;
        let chunk_delay = self.chunk_delay;

        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let stream = stream! {
                yield ModelEvent::TurnStarted { message_id: message_id.clone(), model: demo_model_identity() };
                match &response {
                    DemoResponse::Tool { call, message } => {
                        yield ModelEvent::ToolCallStarted { id: call.id.clone(), name: call.name.clone() };
                        yield ModelEvent::ToolCallDelta { id: call.id.clone(), arguments_delta: call.arguments.to_string() };
                        yield ModelEvent::ToolCallFinished { id: call.id.clone(), arguments: call.arguments.clone() };
                        yield ModelEvent::UsageUpdated { usage: message.usage.clone().expect("demo usage exists") };
                        yield ModelEvent::TurnFinished { message: message.clone() };
                    }
                    DemoResponse::Text { reasoning, text, message } => {
                        let reasoning_id = valid_part_id(&format!("demo-reasoning-{sequence}"));
                        let text_id = valid_part_id(&format!("demo-text-{sequence}"));
                        yield ModelEvent::ReasoningStarted { id: reasoning_id.clone() };
                        for delta in text_chunks(reasoning, 12) {
                            tokio::select! {
                                () = cancellation.cancelled() => { yield ModelEvent::TurnFailed { error: ModelError::Cancelled }; return; }
                                () = tokio::time::sleep(chunk_delay) => {}
                            }
                            yield ModelEvent::ReasoningDelta { id: reasoning_id.clone(), delta };
                        }
                        yield ModelEvent::ReasoningFinished { id: reasoning_id };
                        yield ModelEvent::TextStarted { id: text_id.clone() };
                        for delta in text_chunks(text, 12) {
                            tokio::select! {
                                () = cancellation.cancelled() => { yield ModelEvent::TurnFailed { error: ModelError::Cancelled }; return; }
                                () = tokio::time::sleep(chunk_delay) => {}
                            }
                            yield ModelEvent::TextDelta { id: text_id.clone(), delta };
                        }
                        yield ModelEvent::TextFinished { id: text_id };
                        yield ModelEvent::UsageUpdated { usage: message.usage.clone().expect("demo usage exists") };
                        yield ModelEvent::TurnFinished { message: message.clone() };
                    }
                }
            };
            Ok(Box::pin(stream) as ModelEventStream)
        })
    }
}

enum DemoResponse {
    Tool {
        call: ToolCall,
        message: AssistantMessage,
    },
    Text {
        reasoning: String,
        text: String,
        message: AssistantMessage,
    },
}

fn response_for(request: &ModelRequest, sequence: u64, user_text: &str) -> DemoResponse {
    let completed_tools = request
        .conversation
        .messages
        .iter()
        .rev()
        .take_while(|message| !matches!(message, ConversationMessage::User(_)))
        .filter(|message| matches!(message, ConversationMessage::Tool(_)))
        .count();
    if let Ok(Some((limit, name, arguments))) = parse_repeat_command(user_text) {
        if completed_tools < limit {
            return tool_response(sequence, user_text, name, arguments);
        }
        return text_response(
            sequence,
            user_text,
            format!("重复工具调用已完成，共收到 {completed_tools} 条结果。"),
        );
    }
    if completed_tools > 0 {
        return text_response(
            sequence,
            user_text,
            format!(
                "工具调用已完成，共收到 {completed_tools} 条结果。请在 Journal 与审计区查看完整结果。"
            ),
        );
    }
    match parse_tool_command(user_text) {
        Ok(Some((name, arguments))) => tool_response(sequence, user_text, name, arguments),
        Ok(None) => text_response(
            sequence,
            user_text,
            format!("Deterministic response: {user_text}"),
        ),
        Err(error) => text_response(sequence, user_text, format!("工具命令格式错误：{error}")),
    }
}

fn tool_response(
    sequence: u64,
    user_text: &str,
    name: ToolName,
    arguments: serde_json::Value,
) -> DemoResponse {
    let call = ToolCall {
        id: ToolCallId::new(format!("demo-call-{sequence}")).expect("generated call id is valid"),
        name,
        arguments,
    };
    let message = AssistantMessage {
        id: valid_message_id(&format!("demo-message-{sequence}")),
        model: demo_model_identity(),
        parts: vec![AssistantPart::ToolCall(call.clone())],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(usage(
            user_text.len() as u64,
            call.arguments.to_string().len() as u64,
        )),
    };
    DemoResponse::Tool { call, message }
}

fn text_response(sequence: u64, user_text: &str, text: String) -> DemoResponse {
    let reasoning =
        "Reading the frozen conversation with the deterministic tool-capable model.".to_owned();
    let message = AssistantMessage {
        id: valid_message_id(&format!("demo-message-{sequence}")),
        model: demo_model_identity(),
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: valid_part_id(&format!("demo-reasoning-{sequence}")),
                text: reasoning.clone(),
            }),
            AssistantPart::Text(TextPart {
                id: valid_part_id(&format!("demo-text-{sequence}")),
                text: text.clone(),
            }),
        ],
        finish_reason: FinishReason::Stop,
        usage: Some(usage(
            user_text.len() as u64,
            (reasoning.len() + text.len()) as u64,
        )),
    };
    DemoResponse::Text {
        reasoning,
        text,
        message,
    }
}

fn parse_tool_command(text: &str) -> Result<Option<(ToolName, serde_json::Value)>, String> {
    let Some(rest) = text.strip_prefix("/tool ") else {
        return Ok(None);
    };
    let mut fields = rest.trim().splitn(2, char::is_whitespace);
    let name = fields
        .next()
        .filter(|name| !name.is_empty())
        .ok_or("缺少工具名")?;
    let arguments = fields
        .next()
        .filter(|value| !value.trim().is_empty())
        .ok_or("缺少 JSON 参数")?;
    let name = ToolName::new(name).map_err(|error| error.to_string())?;
    let arguments = serde_json::from_str(arguments).map_err(|error| error.to_string())?;
    Ok(Some((name, arguments)))
}

fn parse_repeat_command(
    text: &str,
) -> Result<Option<(usize, ToolName, serde_json::Value)>, String> {
    let Some(rest) = text.strip_prefix("/repeat ") else {
        return Ok(None);
    };
    let mut fields = rest.trim().splitn(3, char::is_whitespace);
    let limit = fields
        .next()
        .ok_or("缺少重复次数")?
        .parse::<usize>()
        .map_err(|_| "重复次数必须是整数")?;
    if limit == 0 || limit > 10 {
        return Err("重复次数必须在 1 到 10 之间".to_owned());
    }
    let name = fields.next().ok_or("缺少工具名")?;
    let arguments = fields.next().ok_or("缺少 JSON 参数")?;
    Ok(Some((
        limit,
        ToolName::new(name).map_err(|error| error.to_string())?,
        serde_json::from_str(arguments).map_err(|error| error.to_string())?,
    )))
}

fn usage(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input.saturating_add(output),
        cached_input_tokens: None,
        reasoning_tokens: None,
    }
}

fn demo_model_identity() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("core-demo").expect("valid provider"),
        "deterministic-m5",
    )
}

fn valid_message_id(value: &str) -> MessageId {
    MessageId::new(value).expect("generated message id is valid")
}
fn valid_part_id(value: &str) -> PartId {
    PartId::new(value).expect("generated part id is valid")
}

fn latest_user_text(request: &ModelRequest) -> String {
    request
        .conversation
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ConversationMessage::User(message) => Some(
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::Text(part) => Some(part.text.as_str()),
                        UserPart::Injected(_) | UserPart::FileReferences(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "(empty user message)".to_owned())
}

fn text_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        current.push(character);
        if current.chars().count() == max_chars {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_tool_command_without_reinterpreting_arguments() {
        let (_, arguments) =
            parse_tool_command(r#"/tool shell {"command":"printf a | sed s/a/b/"}"#)
                .expect("parse")
                .expect("tool");
        assert_eq!(arguments["command"], "printf a | sed s/a/b/");
    }

    #[test]
    fn chunks_preserve_unicode_text() {
        let text = "你好 deterministic world";
        assert_eq!(text_chunks(text, 4).concat(), text);
    }
}
