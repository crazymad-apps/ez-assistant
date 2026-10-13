//! 显式 JSON 辅助操作；不进入生成重试、SSE 或工具执行生命周期。

use std::time::Duration;

use agent_model::{ContextPolicy, ModelCallContext, ModelError, ModelRequest};
use agent_types::{
    AssistantMessage, AssistantMessagePurpose, AssistantPart, FinishReason, MessageId,
    ModelIdentity, OpaqueProviderState, ProviderContextEffect,
};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{OpenAiResponsesService, classify_http_error, map_transport_error};
use crate::responses::{encode::encode_request_with_images, schema::ResponsesUsage};
use crate::{TransportRequest, shared::join_endpoint};

pub(in crate::responses) const STATE_TYPE: &str = "responses.compacted_window";
// 整体 JSON 响应预算与普通 reasoning item/turn 的持久化预算分开。
const JSON_BODY_LIMIT: usize = 32 * 1024 * 1024;

impl OpenAiResponsesService {
    pub(super) fn prepare_context_request(
        &self,
        mut request: ModelRequest,
    ) -> Result<ModelRequest, ModelError> {
        if let ContextPolicy::ServerCompaction { .. } = &self.context_policy {
            request.conversation = request
                .conversation
                .project_provider_context()
                .map_err(|_| ModelError::Protocol("invalid native context projection".into()))?;
            for message in &request.conversation.messages {
                let agent_types::ConversationMessage::Assistant(assistant) = message else {
                    continue;
                };
                if assistant.parts.iter().any(|part| {
                    matches!(part, AssistantPart::ProviderState(state)
                        if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext)
                            && (state.provider() != &self.adapter.provider
                                || state.protocol() != &self.adapter.protocol))
                }) {
                    return Err(protocol("native window uses an incompatible provider or protocol"));
                }
            }
        }
        Ok(request)
    }

    pub(super) async fn count_tokens(
        &self,
        request: &ModelRequest,
        context: &mut ModelCallContext,
    ) -> Result<Option<u64>, ModelError> {
        if context.cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        if !self.input_token_count_supported {
            return Ok(None);
        }
        let request = self.prepare_context_request(request.clone())?;
        let encoded = encode_request_with_images(
            &request,
            &context.prepared_images,
            &self.adapter,
            &self.model,
        )?;
        let mut body =
            serde_json::to_value(encoded).map_err(|_| protocol("input count encoding failed"))?;
        let object = body
            .as_object_mut()
            .ok_or_else(|| protocol("input count encoding failed"))?;
        for key in [
            "stream",
            "store",
            "previous_response_id",
            "max_output_tokens",
            "temperature",
            "top_p",
        ] {
            object.remove(key);
        }
        // 展示计量有独立的短预算，不能把这个总时限施加到付费压缩操作。
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            self.json_operation("responses/input_tokens", body, context),
        )
        .await
        .map_err(|_| ModelError::Transport {
            kind: agent_model::ModelTransportErrorKind::Timeout,
            message: "input token count request timed out".into(),
        })??;
        response
            .get("input_tokens")
            .and_then(Value::as_u64)
            .map(Some)
            .ok_or_else(|| protocol("input count response lacks a valid input_tokens value"))
    }

    pub(super) async fn compact_context(
        &self,
        request: ModelRequest,
        context: ModelCallContext,
    ) -> Result<AssistantMessage, ModelError> {
        if context.cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let ContextPolicy::ServerCompaction {
            state_scope_fingerprint,
            ..
        } = &self.context_policy
        else {
            return Err(ModelError::Config("server compaction is disabled".into()));
        };
        let request = self.prepare_context_request(request)?;
        let encoded = encode_request_with_images(
            &request,
            &context.prepared_images,
            &self.adapter,
            &self.model,
        )?;
        let mut body = json!({"model": encoded.model, "input": encoded.input});
        if let Some(instructions) = encoded.instructions {
            body["instructions"] = Value::String(instructions);
        }
        // 压缩沿用宿主配置的 Transport 响应建立/正文空闲超时，不另设 120 秒总时限。
        let response = self
            .json_operation("responses/compact", body, &context)
            .await?;
        validate_window(&response)?;
        let message_id = MessageId::new(required_string(&response, "id")?)
            .map_err(|_| protocol("compact response ID is invalid"))?;
        let usage = response
            .get("usage")
            .filter(|usage| !usage.is_null())
            .map(|usage| {
                serde_json::from_value::<ResponsesUsage>(usage.clone())
                    .map(|usage| crate::responses::stream::map_usage(&usage))
                    .map_err(|_| protocol("compact usage is invalid"))
            })
            .transpose()?;
        let payload =
            serde_json::to_vec(&response).map_err(|_| protocol("compact state encoding failed"))?;
        let state = OpaqueProviderState::new_context_replacement(
            self.adapter.provider.clone(),
            self.adapter.protocol.clone(),
            STATE_TYPE,
            "application/json",
            1,
            state_scope_fingerprint.clone(),
            payload,
        )
        .map_err(|_| protocol("compact state exceeds its limits or has an invalid binding"))?;
        Ok(AssistantMessage {
            id: message_id,
            purpose: AssistantMessagePurpose::ContextCompaction,
            model: ModelIdentity::new(self.adapter.provider.clone(), self.model.clone()),
            parts: vec![AssistantPart::ProviderState(state)],
            finish_reason: FinishReason::Stop,
            usage,
        })
    }

    async fn json_operation(
        &self,
        endpoint: &str,
        body: Value,
        context: &ModelCallContext,
    ) -> Result<Value, ModelError> {
        let operation = async {
            let request = TransportRequest {
                trace: context.trace.clone(),
                method: "POST".into(),
                url: join_endpoint(&self.base_url, endpoint),
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    ("accept".into(), "application/json".into()),
                    (
                        "authorization".into(),
                        self.credential.authorization_header(),
                    ),
                ],
                body: serde_json::to_vec(&body)
                    .map_err(|_| protocol("auxiliary request encoding failed"))?,
            };
            let mut response = self
                .transport
                .execute(request)
                .await
                .map_err(map_transport_error)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.body.next().await {
                let chunk = chunk.map_err(map_transport_error)?;
                if bytes.len().saturating_add(chunk.len()) > JSON_BODY_LIMIT {
                    return Err(protocol(
                        "auxiliary JSON response exceeds the 32 MiB transport budget",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            if !(200..300).contains(&response.status) {
                return Err(classify_http_error(
                    response.status,
                    &response.headers,
                    &bytes,
                ));
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| protocol("auxiliary response is not one complete JSON object"))
        };
        tokio::select! {
            () = context.cancellation.cancelled() => Err(ModelError::Cancelled),
            result = operation => result,
        }
    }
}

pub(in crate::responses) fn window_items(
    state: &OpaqueProviderState,
) -> Result<Vec<Value>, ModelError> {
    if state.format_version() != 1
        || state.media_type() != "application/json"
        || state.context_effect() != Some(ProviderContextEffect::ReplacePriorContext)
    {
        return Err(protocol("compact state format is invalid"));
    }
    let value: Value = serde_json::from_slice(state.payload())
        .map_err(|_| protocol("compact state is not valid JSON"))?;
    validate_window(&value)?;
    Ok(value["output"]
        .as_array()
        .ok_or_else(|| protocol("compact output is missing"))?
        .clone())
}

fn validate_window(value: &Value) -> Result<(), ModelError> {
    if value.get("object").and_then(Value::as_str) != Some("response.compaction")
        || value
            .get("status")
            .is_some_and(|status| status != "completed")
    {
        return Err(protocol(
            "compact response is not a complete successful object",
        ));
    }
    let id = required_string(value, "id")?;
    if id.len() > 1024 || id.chars().any(char::is_control) {
        return Err(protocol("compact ID exceeds its limits"));
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .filter(|output| !output.is_empty())
        .ok_or_else(|| protocol("compact output is missing"))?;
    let mut native_seen = false;
    let mut validation = Vec::new();
    let identity = ModelIdentity::new(
        agent_types::ProviderId::new("validation").expect("static provider"),
        "native-window",
    );
    let mut previous_kind = "";
    for (index, item) in output.iter().enumerate() {
        let id = MessageId::new(format!("window-item-{index}")).expect("nonempty derived ID");
        if item
            .get("status")
            .is_some_and(|status| status != "completed")
        {
            return Err(protocol("compact window contains an unfinished item"));
        }
        let kind = required_string(item, "type")?;
        match kind {
            "compaction" => {
                required_string(item, "encrypted_content")?;
                native_seen = true;
                let state = OpaqueProviderState::new_routed(
                    identity.provider.clone(),
                    agent_types::ProtocolId::new("openai.responses").expect("static protocol"),
                    STATE_TYPE,
                    "application/json",
                    1,
                    None,
                    "a".repeat(64),
                    vec![],
                )
                .and_then(|state| {
                    state.with_context_effect(ProviderContextEffect::ReplacePriorContext)
                })
                .map_err(|_| protocol("native validation boundary is invalid"))?;
                validation.push(agent_types::ConversationMessage::Assistant(
                    AssistantMessage {
                        id,
                        purpose: AssistantMessagePurpose::ContextCompaction,
                        model: identity.clone(),
                        parts: vec![AssistantPart::ProviderState(state)],
                        finish_reason: FinishReason::Stop,
                        usage: None,
                    },
                ));
            }
            "message" => {
                if !matches!(
                    required_string(item, "role")?,
                    "user" | "assistant" | "system" | "developer"
                ) {
                    return Err(protocol("compact message role is unsupported"));
                }
                validate_content(
                    item.get("content")
                        .ok_or_else(|| protocol("compact message content is missing"))?,
                )?;
            }
            "reasoning" => {
                if item
                    .get("encrypted_content")
                    .is_some_and(|content| !content.is_null())
                {
                    required_string(item, "encrypted_content")?;
                } else if !item.get("summary").is_some_and(Value::is_array)
                    && !item.get("content").is_some_and(Value::is_array)
                {
                    return Err(protocol("compact reasoning item is invalid"));
                }
            }
            "function_call" => {
                let call = agent_types::ToolCall {
                    id: agent_types::ToolCallId::new(required_string(item, "call_id")?)
                        .map_err(|_| protocol("compact call ID is invalid"))?,
                    name: agent_types::ToolName::new(required_string(item, "name")?)
                        .map_err(|_| protocol("compact tool name is invalid"))?,
                    arguments: serde_json::from_str::<Value>(required_string(item, "arguments")?)
                        .map_err(|_| protocol("compact tool arguments are invalid"))?,
                };
                if previous_kind == "function_call" {
                    let Some(agent_types::ConversationMessage::Assistant(assistant)) =
                        validation.last_mut()
                    else {
                        return Err(protocol("compact batch is invalid"));
                    };
                    assistant.parts.push(AssistantPart::ToolCall(call));
                } else {
                    validation.push(agent_types::ConversationMessage::Assistant(
                        AssistantMessage {
                            id,
                            purpose: AssistantMessagePurpose::ModelResponse,
                            model: identity.clone(),
                            parts: vec![AssistantPart::ToolCall(call)],
                            finish_reason: FinishReason::ToolCalls,
                            usage: None,
                        },
                    ));
                }
            }
            "function_call_output" => {
                let content = item
                    .get("output")
                    .ok_or_else(|| protocol("compact tool output is missing"))?;
                if !content.is_string() {
                    validate_content(content)?;
                }
                validation.push(agent_types::ConversationMessage::Tool(
                    agent_types::ToolMessage {
                        id,
                        result: agent_types::ToolResult {
                            call_id: agent_types::ToolCallId::new(required_string(
                                item, "call_id",
                            )?)
                            .map_err(|_| protocol("compact result ID is invalid"))?,
                            status: agent_types::ToolResultStatus::Success,
                            content: agent_types::ToolResultContent::text("validation only"),
                            metadata: None,
                        },
                    },
                ));
            }
            _ => return Err(protocol("compact window contains an unsupported item")),
        }
        // 普通非工具项也是配对边界，不让 reasoning/message 隐藏半组调用。
        if matches!(kind, "message" | "reasoning") {
            validation.push(agent_types::ConversationMessage::System(
                agent_types::SystemMessage {
                    id: MessageId::new(format!("window-boundary-{index}")).expect("derived ID"),
                    text: String::new(),
                },
            ));
        }
        previous_kind = kind;
    }
    agent_types::ConversationSnapshot::new(validation)
        .validate_tool_exchange_pairs()
        .map_err(|_| protocol("compact window has an invalid tool exchange"))?;
    if !native_seen {
        return Err(protocol("compact window lacks a complete native boundary"));
    }
    Ok(())
}

fn validate_content(value: &Value) -> Result<(), ModelError> {
    if value.is_string() {
        return Ok(());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| protocol("compact content is invalid"))?;
    for part in parts {
        match required_string(part, "type")? {
            "input_text" | "output_text" => {
                part.get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol("compact text is invalid"))?;
            }
            "input_image" => {
                required_string(part, "image_url")?;
            }
            "refusal" => {
                required_string(part, "refusal")?;
            }
            _ => return Err(protocol("compact content type is unsupported")),
        }
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol("compact object lacks a required string"))
}

fn protocol(message: &str) -> ModelError {
    ModelError::Protocol(message.into())
}
