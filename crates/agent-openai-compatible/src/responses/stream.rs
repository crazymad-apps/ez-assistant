use std::collections::{BTreeMap, HashMap};

use agent_model::{ModelError, ModelEvent};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MAX_PROVIDER_STATE_ITEM_BYTES,
    MAX_PROVIDER_STATE_TURN_BYTES, MessageId, ModelIdentity, OpaqueProviderState, PartId,
    ReasoningPart, TextPart, TokenUsage, ToolCall, ToolCallId, ToolName,
};
use serde_json::Value;

use super::{ResponsesProtocolAdapter, adapter::ReasoningTextProjection, schema::ResponsesUsage};

/// Responses SSE item/event 的有序状态机。
pub(super) struct ResponsesAssembler {
    adapter: ResponsesProtocolAdapter,
    configured_model: String,
    response_id: Option<String>,
    message_id: Option<MessageId>,
    response_model: Option<String>,
    items: BTreeMap<u64, OutputItem>,
    item_indices: HashMap<String, u64>,
    usage: Option<TokenUsage>,
    started: bool,
    terminal: bool,
    saw_refusal: bool,
    saw_tool_call: bool,
    provider_state_bytes: usize,
}

struct OutputItem {
    item_id: String,
    done: bool,
    kind: OutputItemKind,
}

enum OutputItemKind {
    Message {
        content: BTreeMap<u64, TextState>,
    },
    FunctionCall {
        call_id: ToolCallId,
        name: ToolName,
        arguments: String,
        arguments_done: bool,
        parsed_arguments: Option<Value>,
    },
    Reasoning {
        segments: BTreeMap<(u8, u64), TextState>,
        opaque_payload: Option<Vec<u8>>,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TextKind {
    Output,
    Refusal,
    Reasoning,
}

struct TextState {
    id: PartId,
    kind: TextKind,
    text: String,
    finished: bool,
}

impl ResponsesAssembler {
    pub(super) fn new(adapter: ResponsesProtocolAdapter, configured_model: String) -> Self {
        Self {
            adapter,
            configured_model,
            response_id: None,
            message_id: None,
            response_model: None,
            items: BTreeMap::new(),
            item_indices: HashMap::new(),
            usage: None,
            started: false,
            terminal: false,
            saw_refusal: false,
            saw_tool_call: false,
            provider_state_bytes: 0,
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub(super) fn push(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        if self.terminal {
            return Err(protocol("event arrived after the Responses terminal event"));
        }
        let event_type = string(event, "type")?;
        match event_type {
            "response.created" => self.response_created(event),
            "response.queued" | "response.in_progress" => {
                self.validate_status_response(event)?;
                Ok(Vec::new())
            }
            "response.output_item.added" => self.output_item_added(event),
            "response.output_item.done" => self.output_item_done(event),
            "response.content_part.added" => self.content_part_added(event),
            "response.content_part.done" => self.content_part_done(event),
            "response.output_text.delta" => self.text_delta(event, TextKind::Output),
            "response.output_text.done" => self.text_done(event, TextKind::Output, "text"),
            "response.refusal.delta" => self.text_delta(event, TextKind::Refusal),
            "response.refusal.done" => self.text_done(event, TextKind::Refusal, "refusal"),
            "response.function_call_arguments.delta" => self.function_arguments_delta(event),
            "response.function_call_arguments.done" => self.function_arguments_done(event),
            "response.reasoning_summary_part.added" => {
                self.reasoning_part_added(event, 0, "summary_index")
            }
            "response.reasoning_summary_part.done" => {
                self.reasoning_part_done(event, 0, "summary_index")
            }
            "response.reasoning_summary_text.delta" => {
                self.reasoning_delta(event, 0, "summary_index")
            }
            "response.reasoning_summary_text.done" => {
                self.reasoning_done(event, 0, "summary_index", "text")
            }
            "response.reasoning_text.delta" => {
                self.reasoning_delta(event, self.reasoning_text_kind(), "content_index")
            }
            "response.reasoning_text.done" => {
                self.reasoning_done(event, self.reasoning_text_kind(), "content_index", "text")
            }
            "response.completed" => self.success_terminal(event, false),
            "response.incomplete" => self.success_terminal(event, true),
            "response.failed" | "response.cancelled" => self.provider_failure(event),
            "error" => self.error_event(event),
            _ => Err(protocol(format!(
                "unsupported Responses event type `{event_type}`"
            ))),
        }
    }

    pub(super) fn finalize(self) -> Result<Vec<ModelEvent>, ModelError> {
        if self.terminal {
            Ok(Vec::new())
        } else {
            Err(protocol("Responses stream ended without a terminal event"))
        }
    }

    fn response_created(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        if self.started {
            return Err(protocol("duplicate response.created event"));
        }
        let response = object_field(event, "response")?;
        let response_id = string(response, "id")?.to_owned();
        let model = optional_string(response, "model")
            .unwrap_or(self.configured_model.as_str())
            .to_owned();
        let message_id = MessageId::new(response_id.clone())
            .map_err(|_| protocol("Responses response id is invalid"))?;
        self.response_id = Some(response_id);
        self.message_id = Some(message_id.clone());
        self.response_model = Some(model.clone());
        self.started = true;
        Ok(vec![ModelEvent::TurnStarted {
            message_id,
            model: ModelIdentity::new(self.adapter.provider.clone(), model),
        }])
    }

    fn validate_status_response(&self, event: &Value) -> Result<(), ModelError> {
        self.require_started()?;
        let response = object_field(event, "response")?;
        self.validate_response_id(response)
    }

    fn output_item_added(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        self.require_started()?;
        let index = integer(event, "output_index")?;
        if self.items.contains_key(&index) {
            return Err(protocol("duplicate Responses output index"));
        }
        let item = object_field(event, "item")?;
        let item_id = string(item, "id")?.to_owned();
        if self.item_indices.insert(item_id.clone(), index).is_some() {
            return Err(protocol("duplicate Responses output item id"));
        }
        let mut events = Vec::new();
        let kind = match string(item, "type")? {
            "message" => OutputItemKind::Message {
                content: BTreeMap::new(),
            },
            "function_call" => {
                let call_id = ToolCallId::new(string(item, "call_id")?.to_owned())
                    .map_err(|_| protocol("Responses function call id is invalid"))?;
                let name = ToolName::new(string(item, "name")?.to_owned())
                    .map_err(|_| protocol("Responses function name is invalid"))?;
                let arguments = optional_string(item, "arguments")
                    .unwrap_or_default()
                    .to_owned();
                events.push(ModelEvent::ToolCallStarted {
                    id: call_id.clone(),
                    name: name.clone(),
                });
                self.saw_tool_call = true;
                OutputItemKind::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    arguments_done: false,
                    parsed_arguments: None,
                }
            }
            "reasoning" => OutputItemKind::Reasoning {
                segments: BTreeMap::new(),
                opaque_payload: None,
            },
            other => {
                return Err(protocol(format!(
                    "unsupported Responses output item type `{other}`"
                )));
            }
        };
        self.items.insert(
            index,
            OutputItem {
                item_id,
                done: false,
                kind,
            },
        );
        Ok(events)
    }

    fn content_part_added(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let output_index = integer(event, "output_index")?;
        let content_index = integer(event, "content_index")?;
        let item_id = string(event, "item_id")?;
        let part = object_field(event, "part")?;
        let kind = match string(part, "type")? {
            "output_text" => TextKind::Output,
            "refusal" => TextKind::Refusal,
            "reasoning_text" => {
                return self.ensure_reasoning_state(
                    output_index,
                    item_id,
                    self.reasoning_text_kind(),
                    content_index,
                );
            }
            other => {
                return Err(protocol(format!(
                    "unsupported Responses message content type `{other}`"
                )));
            }
        };
        self.ensure_text_state(output_index, item_id, content_index, kind)
    }

    fn content_part_done(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let part = object_field(event, "part")?;
        let kind = match string(part, "type")? {
            "output_text" => TextKind::Output,
            "refusal" => TextKind::Refusal,
            "reasoning_text" => {
                return self.finish_reasoning_text(
                    integer(event, "output_index")?,
                    string(event, "item_id")?,
                    self.reasoning_text_kind(),
                    integer(event, "content_index")?,
                    optional_string(part, "text").unwrap_or_default(),
                );
            }
            other => {
                return Err(protocol(format!(
                    "unsupported Responses message content type `{other}`"
                )));
            }
        };
        let field = if kind == TextKind::Refusal {
            "refusal"
        } else {
            "text"
        };
        self.finish_message_text(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            integer(event, "content_index")?,
            kind,
            optional_string(part, field).unwrap_or_default(),
        )
    }

    fn text_delta(&mut self, event: &Value, kind: TextKind) -> Result<Vec<ModelEvent>, ModelError> {
        let output_index = integer(event, "output_index")?;
        let content_index = integer(event, "content_index")?;
        let item_id = string(event, "item_id")?;
        let delta = string(event, "delta")?.to_owned();
        let mut events = self.ensure_text_state(output_index, item_id, content_index, kind)?;
        let state = self.message_text_mut(output_index, item_id, content_index, kind)?;
        if state.finished {
            return Err(protocol("Responses text delta arrived after text done"));
        }
        state.text.push_str(&delta);
        events.push(text_delta_event(state.kind, state.id.clone(), delta));
        Ok(events)
    }

    fn text_done(
        &mut self,
        event: &Value,
        kind: TextKind,
        field: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.finish_message_text(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            integer(event, "content_index")?,
            kind,
            string(event, field)?,
        )
    }

    fn ensure_text_state(
        &mut self,
        output_index: u64,
        item_id: &str,
        content_index: u64,
        kind: TextKind,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("Responses text references an unknown output item"))?;
        let OutputItemKind::Message { content } = &mut item.kind else {
            return Err(protocol(
                "Responses text references a non-message output item",
            ));
        };
        if let Some(state) = content.get(&content_index) {
            if state.kind != kind {
                return Err(protocol("Responses content type changed during streaming"));
            }
            return Ok(Vec::new());
        }
        let id = PartId::new(format!("{item_id}:content:{content_index}"))
            .map_err(|_| protocol("Responses content part id is invalid"))?;
        content.insert(
            content_index,
            TextState {
                id: id.clone(),
                kind,
                text: String::new(),
                finished: false,
            },
        );
        if kind == TextKind::Refusal {
            self.saw_refusal = true;
        }
        Ok(vec![ModelEvent::TextStarted { id }])
    }

    fn finish_message_text(
        &mut self,
        output_index: u64,
        item_id: &str,
        content_index: u64,
        kind: TextKind,
        final_text: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = self.ensure_text_state(output_index, item_id, content_index, kind)?;
        let state = self.message_text_mut(output_index, item_id, content_index, kind)?;
        finish_text_state(state, final_text, &mut events)?;
        Ok(events)
    }

    fn message_text_mut(
        &mut self,
        output_index: u64,
        item_id: &str,
        content_index: u64,
        kind: TextKind,
    ) -> Result<&mut TextState, ModelError> {
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("Responses text references an unknown output item"))?;
        let OutputItemKind::Message { content } = &mut item.kind else {
            return Err(protocol(
                "Responses text references a non-message output item",
            ));
        };
        let state = content
            .get_mut(&content_index)
            .ok_or_else(|| protocol("Responses content part was not initialized"))?;
        if state.kind != kind {
            return Err(protocol("Responses content type changed during streaming"));
        }
        Ok(state)
    }

    fn function_arguments_delta(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let output_index = integer(event, "output_index")?;
        let item_id = string(event, "item_id")?;
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("function arguments reference an unknown item"))?;
        let OutputItemKind::FunctionCall {
            call_id,
            arguments,
            arguments_done,
            ..
        } = &mut item.kind
        else {
            return Err(protocol("function arguments reference a non-function item"));
        };
        if *arguments_done {
            return Err(protocol("function arguments delta arrived after done"));
        }
        let delta = string(event, "delta")?.to_owned();
        arguments.push_str(&delta);
        Ok(vec![ModelEvent::ToolCallDelta {
            id: call_id.clone(),
            arguments_delta: delta,
        }])
    }

    fn function_arguments_done(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        self.finish_function_arguments(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            string(event, "arguments")?,
        )
    }

    fn finish_function_arguments(
        &mut self,
        output_index: u64,
        item_id: &str,
        final_arguments: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("function arguments reference an unknown item"))?;
        let OutputItemKind::FunctionCall {
            call_id,
            arguments,
            arguments_done,
            parsed_arguments,
            ..
        } = &mut item.kind
        else {
            return Err(protocol("function arguments reference a non-function item"));
        };
        if *arguments_done {
            if arguments == final_arguments {
                return Ok(Vec::new());
            }
            return Err(protocol("function arguments changed after done"));
        }
        let mut events = Vec::new();
        append_final_text(arguments, final_arguments, |delta| {
            events.push(ModelEvent::ToolCallDelta {
                id: call_id.clone(),
                arguments_delta: delta,
            });
        })?;
        let parsed = serde_json::from_str::<Value>(arguments).map_err(|error| {
            ModelError::ToolArguments(format!(
                "Responses function call `{call_id}` returned invalid JSON: {error}"
            ))
        })?;
        *arguments_done = true;
        *parsed_arguments = Some(parsed.clone());
        events.push(ModelEvent::ToolCallFinished {
            id: call_id.clone(),
            arguments: parsed,
        });
        Ok(events)
    }

    fn reasoning_part_added(
        &mut self,
        event: &Value,
        kind: u8,
        index_field: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.ensure_reasoning_state(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            kind,
            optional_integer(event, index_field).unwrap_or(0),
        )
    }

    fn reasoning_part_done(
        &mut self,
        event: &Value,
        kind: u8,
        index_field: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let part = object_field(event, "part")?;
        self.finish_reasoning_text(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            kind,
            optional_integer(event, index_field).unwrap_or(0),
            optional_string(part, "text").unwrap_or_default(),
        )
    }

    fn reasoning_delta(
        &mut self,
        event: &Value,
        kind: u8,
        index_field: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let output_index = integer(event, "output_index")?;
        let item_id = string(event, "item_id")?;
        let segment_index = optional_integer(event, index_field).unwrap_or(0);
        let delta = string(event, "delta")?.to_owned();
        let mut events = self.ensure_reasoning_state(output_index, item_id, kind, segment_index)?;
        let state = self.reasoning_state_mut(output_index, item_id, kind, segment_index)?;
        if state.finished {
            return Err(protocol("reasoning delta arrived after done"));
        }
        state.text.push_str(&delta);
        events.push(ModelEvent::ReasoningDelta {
            id: state.id.clone(),
            delta,
        });
        Ok(events)
    }

    fn reasoning_done(
        &mut self,
        event: &Value,
        kind: u8,
        index_field: &str,
        text_field: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.finish_reasoning_text(
            integer(event, "output_index")?,
            string(event, "item_id")?,
            kind,
            optional_integer(event, index_field).unwrap_or(0),
            string(event, text_field)?,
        )
    }

    fn ensure_reasoning_state(
        &mut self,
        output_index: u64,
        item_id: &str,
        kind: u8,
        segment_index: u64,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("reasoning references an unknown output item"))?;
        let OutputItemKind::Reasoning { segments, .. } = &mut item.kind else {
            return Err(protocol("reasoning event references a non-reasoning item"));
        };
        if let Some(state) = segments.get(&(kind, segment_index)) {
            if state.kind != TextKind::Reasoning {
                return Err(protocol("reasoning segment type changed"));
            }
            return Ok(Vec::new());
        }
        let segment_name = if kind == 0 { "summary" } else { "content" };
        let id = PartId::new(format!("{item_id}:{segment_name}:{segment_index}"))
            .map_err(|_| protocol("Responses reasoning part id is invalid"))?;
        segments.insert(
            (kind, segment_index),
            TextState {
                id: id.clone(),
                kind: TextKind::Reasoning,
                text: String::new(),
                finished: false,
            },
        );
        Ok(vec![ModelEvent::ReasoningStarted { id }])
    }

    fn finish_reasoning_text(
        &mut self,
        output_index: u64,
        item_id: &str,
        kind: u8,
        segment_index: u64,
        final_text: &str,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        let mut events = self.ensure_reasoning_state(output_index, item_id, kind, segment_index)?;
        let state = self.reasoning_state_mut(output_index, item_id, kind, segment_index)?;
        finish_text_state(state, final_text, &mut events)?;
        Ok(events)
    }

    fn reasoning_state_mut(
        &mut self,
        output_index: u64,
        item_id: &str,
        kind: u8,
        segment_index: u64,
    ) -> Result<&mut TextState, ModelError> {
        self.validate_item_identity(output_index, item_id)?;
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("reasoning references an unknown output item"))?;
        let OutputItemKind::Reasoning { segments, .. } = &mut item.kind else {
            return Err(protocol("reasoning event references a non-reasoning item"));
        };
        segments
            .get_mut(&(kind, segment_index))
            .ok_or_else(|| protocol("reasoning segment was not initialized"))
    }

    fn output_item_done(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let output_index = integer(event, "output_index")?;
        let item_value = object_field(event, "item")?;
        let item_id = string(item_value, "id")?;
        self.validate_item_identity(output_index, item_id)?;
        if self.items.get(&output_index).is_some_and(|item| item.done) {
            return Err(protocol("duplicate Responses output_item.done event"));
        }

        let mut events = Vec::new();
        match string(item_value, "type")? {
            "message" => {
                for (content_index, part) in array_field(item_value, "content")?.iter().enumerate()
                {
                    let kind = match string(part, "type")? {
                        "output_text" => TextKind::Output,
                        "refusal" => TextKind::Refusal,
                        other => {
                            return Err(protocol(format!(
                                "unsupported Responses message content type `{other}`"
                            )));
                        }
                    };
                    let field = if kind == TextKind::Refusal {
                        "refusal"
                    } else {
                        "text"
                    };
                    events.extend(
                        self.finish_message_text(
                            output_index,
                            item_id,
                            u64::try_from(content_index)
                                .map_err(|_| protocol("Responses content index overflow"))?,
                            kind,
                            string(part, field)?,
                        )?,
                    );
                }
            }
            "function_call" => {
                events.extend(self.finish_function_arguments(
                    output_index,
                    item_id,
                    string(item_value, "arguments")?,
                )?);
            }
            "reasoning" => {
                if let Some(summary) = item_value.get("summary").and_then(Value::as_array) {
                    for (summary_index, part) in summary.iter().enumerate() {
                        events.extend(
                            self.finish_reasoning_text(
                                output_index,
                                item_id,
                                0,
                                u64::try_from(summary_index)
                                    .map_err(|_| protocol("reasoning summary index overflow"))?,
                                string(part, "text")?,
                            )?,
                        );
                    }
                }
                if let Some(content) = item_value.get("content").and_then(Value::as_array) {
                    for (content_index, part) in content.iter().enumerate() {
                        if string(part, "type")? != "reasoning_text" {
                            return Err(protocol("unsupported Responses reasoning content part"));
                        }
                        events.extend(
                            self.finish_reasoning_text(
                                output_index,
                                item_id,
                                self.reasoning_text_kind(),
                                u64::try_from(content_index)
                                    .map_err(|_| protocol("reasoning content index overflow"))?,
                                string(part, "text")?,
                            )?,
                        );
                    }
                }
                self.capture_opaque_reasoning(output_index, item_value)?;
            }
            other => {
                return Err(protocol(format!(
                    "unsupported Responses output item type `{other}`"
                )));
            }
        }
        let item = self
            .items
            .get_mut(&output_index)
            .expect("validated output item exists");
        if !item_all_parts_finished(item) {
            return Err(protocol("Responses output item finished with an open part"));
        }
        item.done = true;
        Ok(events)
    }

    /// 把通用 `reasoning_text` 事件投影到当前方言声明的最终 item 字段。
    ///
    /// DashScope Qwen 通过 `reasoning_text` 流式发送内容，却在 `output_item.done` 中把同一内容
    /// 放入 `summary`；从首个增量开始使用同一 segment，避免生成两份规范 ReasoningPart。
    fn reasoning_text_kind(&self) -> u8 {
        match self.adapter.reasoning_text_projection {
            ReasoningTextProjection::Content => 1,
            ReasoningTextProjection::Summary => 0,
        }
    }

    fn success_terminal(
        &mut self,
        event: &Value,
        incomplete: bool,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        self.require_started()?;
        let response = object_field(event, "response")?;
        self.validate_response_id(response)?;
        if self.items.values().any(|item| !item.done) {
            return Err(protocol(
                "Responses terminal event arrived before every output item was done",
            ));
        }
        if self
            .items
            .keys()
            .copied()
            .enumerate()
            .any(|(expected, actual)| u64::try_from(expected).ok() != Some(actual))
        {
            return Err(protocol("Responses output indices are not contiguous"));
        }

        let mut events = Vec::new();
        if let Some(usage_value) = response.get("usage").filter(|value| !value.is_null()) {
            let usage: ResponsesUsage = serde_json::from_value(usage_value.clone())
                .map_err(|_| protocol("Responses usage has an invalid shape"))?;
            let usage = map_usage(&usage);
            self.usage = Some(usage.clone());
            events.push(ModelEvent::UsageUpdated { usage });
        }
        let finish_reason = if incomplete {
            let reason = response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if matches!(reason, "max_output_tokens" | "max_tokens") {
                FinishReason::Length
            } else {
                FinishReason::Other(format!("incomplete:{reason}"))
            }
        } else if self.saw_refusal {
            FinishReason::ContentFilter
        } else if self.saw_tool_call {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        let parts = self.collect_parts()?;
        let id = self
            .message_id
            .clone()
            .ok_or_else(|| protocol("Responses turn has no message id"))?;
        let model = self
            .response_model
            .clone()
            .unwrap_or_else(|| self.configured_model.clone());
        events.push(ModelEvent::TurnFinished {
            message: AssistantMessage {
                id,
                model: ModelIdentity::new(self.adapter.provider.clone(), model),
                parts,
                finish_reason,
                usage: self.usage.clone(),
            },
        });
        self.terminal = true;
        Ok(events)
    }

    fn provider_failure(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let response = object_field(event, "response")?;
        if self.started {
            self.validate_response_id(response)?;
        }
        let message = response
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("provider reported a failed Responses turn")
            .to_owned();
        self.terminal = true;
        Ok(vec![ModelEvent::TurnFailed {
            error: ModelError::Provider {
                message,
                status: None,
            },
        }])
    }

    fn error_event(&mut self, event: &Value) -> Result<Vec<ModelEvent>, ModelError> {
        let message = optional_string(event, "message")
            .or_else(|| event.pointer("/error/message").and_then(Value::as_str))
            .unwrap_or("provider emitted a Responses error event")
            .to_owned();
        self.terminal = true;
        Ok(vec![ModelEvent::TurnFailed {
            error: ModelError::Provider {
                message,
                status: None,
            },
        }])
    }

    fn collect_parts(&self) -> Result<Vec<AssistantPart>, ModelError> {
        let mut parts = Vec::new();
        for item in self.items.values() {
            match &item.kind {
                OutputItemKind::Message { content } => {
                    for state in content.values() {
                        parts.push(AssistantPart::Text(TextPart {
                            id: state.id.clone(),
                            text: state.text.clone(),
                        }));
                    }
                }
                OutputItemKind::FunctionCall {
                    call_id,
                    name,
                    parsed_arguments,
                    ..
                } => parts.push(AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: parsed_arguments.clone().ok_or_else(|| {
                        protocol("Responses function call has no finalized arguments")
                    })?,
                })),
                OutputItemKind::Reasoning {
                    segments,
                    opaque_payload,
                } => {
                    for state in segments.values() {
                        parts.push(AssistantPart::Reasoning(ReasoningPart {
                            id: state.id.clone(),
                            text: state.text.clone(),
                        }));
                    }
                    if let Some(payload) = opaque_payload {
                        let related_part_id = segments
                            .values()
                            .next()
                            .map(|state| state.id.clone())
                            .ok_or_else(|| {
                                protocol("opaque reasoning item has no normalized reasoning part")
                            })?;
                        let fingerprint =
                            self.adapter.route_fingerprint.clone().ok_or_else(|| {
                                protocol("opaque reasoning item has no bound route fingerprint")
                            })?;
                        let state = OpaqueProviderState::new_routed(
                            self.adapter.provider.clone(),
                            self.adapter.protocol.clone(),
                            "responses.reasoning_item",
                            "application/json",
                            1,
                            related_part_id,
                            fingerprint,
                            payload.clone(),
                        )
                        .map_err(|_| protocol("opaque reasoning item violates state limits"))?;
                        parts.push(AssistantPart::ProviderState(state));
                    }
                }
            }
        }
        Ok(parts)
    }

    fn capture_opaque_reasoning(
        &mut self,
        output_index: u64,
        item_value: &Value,
    ) -> Result<(), ModelError> {
        let encrypted = item_value.get("encrypted_content");
        let has_encrypted = encrypted
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty());
        if encrypted.is_none_or(Value::is_null) {
            return Ok(());
        }
        if !has_encrypted {
            return Err(protocol(
                "Responses reasoning encrypted content has an invalid shape",
            ));
        }
        if self.adapter.opaque_reasoning
            != super::adapter::OpaqueReasoningPolicy::PreserveEncryptedItem
        {
            return Err(protocol(
                "Responses route returned unconfigured opaque reasoning state",
            ));
        }
        if self.adapter.route_fingerprint.is_none() {
            return Err(protocol(
                "Responses opaque reasoning route was not bound by the service",
            ));
        }
        let payload = serde_json::to_vec(item_value)
            .map_err(|_| protocol("Responses reasoning item could not be serialized"))?;
        if payload.len() > MAX_PROVIDER_STATE_ITEM_BYTES {
            return Err(protocol(
                "Responses reasoning item exceeds the provider-state byte limit",
            ));
        }
        self.provider_state_bytes = self
            .provider_state_bytes
            .checked_add(payload.len())
            .ok_or_else(|| protocol("Responses provider-state byte count overflow"))?;
        if self.provider_state_bytes > MAX_PROVIDER_STATE_TURN_BYTES {
            return Err(protocol(
                "Responses turn exceeds the provider-state byte limit",
            ));
        }
        let item = self
            .items
            .get_mut(&output_index)
            .ok_or_else(|| protocol("reasoning state references an unknown item"))?;
        let OutputItemKind::Reasoning { opaque_payload, .. } = &mut item.kind else {
            return Err(protocol(
                "opaque reasoning state references a non-reasoning item",
            ));
        };
        *opaque_payload = Some(payload);
        Ok(())
    }

    fn validate_item_identity(&self, output_index: u64, item_id: &str) -> Result<(), ModelError> {
        let item = self
            .items
            .get(&output_index)
            .ok_or_else(|| protocol("Responses event references an unknown output index"))?;
        if item.item_id != item_id || self.item_indices.get(item_id).copied() != Some(output_index)
        {
            return Err(protocol("Responses output item identity changed"));
        }
        Ok(())
    }

    fn validate_response_id(&self, response: &Value) -> Result<(), ModelError> {
        let id = string(response, "id")?;
        if self.response_id.as_deref() != Some(id) {
            return Err(protocol("Responses response id changed during streaming"));
        }
        Ok(())
    }

    fn require_started(&self) -> Result<(), ModelError> {
        if self.started {
            Ok(())
        } else {
            Err(protocol("Responses event arrived before response.created"))
        }
    }
}

fn item_all_parts_finished(item: &OutputItem) -> bool {
    match &item.kind {
        OutputItemKind::Message { content } => content.values().all(|state| state.finished),
        OutputItemKind::FunctionCall { arguments_done, .. } => *arguments_done,
        OutputItemKind::Reasoning { segments, .. } => segments.values().all(|state| state.finished),
    }
}

fn finish_text_state(
    state: &mut TextState,
    final_text: &str,
    events: &mut Vec<ModelEvent>,
) -> Result<(), ModelError> {
    if state.finished {
        if state.text == final_text {
            return Ok(());
        }
        return Err(protocol("Responses text changed after done"));
    }
    append_final_text(&mut state.text, final_text, |delta| {
        events.push(text_delta_event(state.kind, state.id.clone(), delta));
    })?;
    state.finished = true;
    events.push(match state.kind {
        TextKind::Reasoning => ModelEvent::ReasoningFinished {
            id: state.id.clone(),
        },
        TextKind::Output | TextKind::Refusal => ModelEvent::TextFinished {
            id: state.id.clone(),
        },
    });
    Ok(())
}

fn append_final_text(
    accumulated: &mut String,
    final_text: &str,
    mut emit_delta: impl FnMut(String),
) -> Result<(), ModelError> {
    if final_text == accumulated {
        return Ok(());
    }
    let Some(remainder) = final_text.strip_prefix(accumulated.as_str()) else {
        return Err(protocol(
            "Responses done text does not match accumulated deltas",
        ));
    };
    if !remainder.is_empty() {
        accumulated.push_str(remainder);
        emit_delta(remainder.to_owned());
    }
    Ok(())
}

fn text_delta_event(kind: TextKind, id: PartId, delta: String) -> ModelEvent {
    match kind {
        TextKind::Reasoning => ModelEvent::ReasoningDelta { id, delta },
        TextKind::Output | TextKind::Refusal => ModelEvent::TextDelta { id, delta },
    }
}

fn map_usage(usage: &ResponsesUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cached_input_tokens: usage
            .input_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens),
        reasoning_tokens: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
    }
}

fn protocol(message: impl Into<String>) -> ModelError {
    ModelError::Protocol(message.into())
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol(format!("Responses field `{field}` must be a string")))
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn integer(value: &Value, field: &str) -> Result<u64, ModelError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol(format!("Responses field `{field}` must be an integer")))
}

fn optional_integer(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

fn object_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value, ModelError> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .ok_or_else(|| protocol(format!("Responses field `{field}` must be an object")))
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], ModelError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| protocol(format!("Responses field `{field}` must be an array")))
}
