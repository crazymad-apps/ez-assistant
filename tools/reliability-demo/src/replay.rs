//! 两层 Replay 共用的私有脚本提取、ProtocolAdapter 重建与错误类型。

use std::collections::BTreeMap;

use agent_model::{ModelCapabilities, ModelError, ModelEvent, ModelRequest};
use agent_openai_compatible::ProtocolAdapter;
use agent_types::ProviderId;
use thiserror::Error;

use crate::trace::{
    LoadedTrace, ModelCallEvent, NativeTracePayload, ProviderMetadata, TraceCompleteness,
};

pub(crate) const OPENAI_COMPATIBLE_ADAPTER: &str = "openai-compatible";
pub(crate) const OPENAI_COMPATIBLE_ADAPTER_VERSION: u32 = 1;
pub(crate) const OPENAI_CHAT_COMPLETIONS_PROTOCOL: &str = "openai.chat_completions";

#[derive(Debug, Error)]
pub(crate) enum ReplayError {
    #[error("exact replay requires a complete trace")]
    IncompleteTrace,
    #[error("unsupported adapter `{adapter}` version {version}")]
    UnsupportedAdapter { adapter: String, version: u32 },
    #[error("unsupported replay protocol_adapter `{0}`")]
    UnsupportedProtocolAdapter(String),
    #[error("invalid replay metadata: {0}")]
    InvalidMetadata(&'static str),
    #[error("corrupt replay script: {0}")]
    CorruptScript(String),
    #[error("{layer} replay request mismatch in `{field}`")]
    RequestMismatch {
        layer: &'static str,
        field: &'static str,
    },
    #[error("{0} replay script is exhausted")]
    ScriptExhausted(&'static str),
    #[error("{layer} replay result differs for correlation `{correlation_id}`")]
    ResultMismatch {
        layer: &'static str,
        correlation_id: String,
    },
    #[error("failed to construct replay model service: {0}")]
    Service(String),
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedModelCall {
    pub(crate) correlation_id: String,
    pub(crate) request: ModelRequest,
    pub(crate) outcome: RecordedModelOutcome,
    pub(crate) events: Vec<ModelEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecordedModelOutcome {
    Established,
    Failed(ModelError),
}

#[derive(Default)]
struct ModelCallBuilder {
    first_sequence: Option<u64>,
    request: Option<ModelRequest>,
    outcome: Option<RecordedModelOutcome>,
    events: Vec<ModelEvent>,
}

pub(crate) fn recorded_model_calls(
    trace: &LoadedTrace,
) -> Result<Vec<RecordedModelCall>, ReplayError> {
    ensure_complete(trace)?;
    let mut calls = BTreeMap::<String, ModelCallBuilder>::new();
    for record in &trace.records {
        let Some(correlation_id) = &record.correlation_id else {
            continue;
        };
        let relevant = matches!(
            record.payload,
            NativeTracePayload::ModelRequest(_)
                | NativeTracePayload::ModelCall(_)
                | NativeTracePayload::ModelEvent(_)
        );
        if !relevant {
            continue;
        }
        let builder = calls.entry(correlation_id.clone()).or_default();
        builder.first_sequence.get_or_insert(record.sequence);
        match &record.payload {
            NativeTracePayload::ModelRequest(request) => {
                if builder.request.replace(request.clone()).is_some() {
                    return Err(ReplayError::CorruptScript(format!(
                        "correlation {correlation_id} has duplicate model requests"
                    )));
                }
            }
            NativeTracePayload::ModelCall(event) => {
                let outcome = match event {
                    ModelCallEvent::StreamEstablished => RecordedModelOutcome::Established,
                    ModelCallEvent::EstablishmentFailed { error } => {
                        RecordedModelOutcome::Failed(error.clone())
                    }
                };
                if builder.outcome.replace(outcome).is_some() {
                    return Err(ReplayError::CorruptScript(format!(
                        "correlation {correlation_id} has duplicate model outcomes"
                    )));
                }
            }
            NativeTracePayload::ModelEvent(event) => builder.events.push(event.clone()),
            _ => {}
        }
    }

    let mut ordered = calls
        .into_iter()
        .map(|(correlation_id, builder)| {
            let first_sequence = builder.first_sequence.ok_or_else(|| {
                ReplayError::CorruptScript(format!("correlation {correlation_id} has no sequence"))
            })?;
            let request = builder.request.ok_or_else(|| {
                ReplayError::CorruptScript(format!(
                    "correlation {correlation_id} has no model request"
                ))
            })?;
            let outcome = builder.outcome.ok_or_else(|| {
                ReplayError::CorruptScript(format!(
                    "correlation {correlation_id} has no model outcome"
                ))
            })?;
            Ok((
                first_sequence,
                RecordedModelCall {
                    correlation_id,
                    request,
                    outcome,
                    events: builder.events,
                },
            ))
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    ordered.sort_by_key(|(sequence, _)| *sequence);
    Ok(ordered.into_iter().map(|(_, call)| call).collect())
}

pub(crate) fn ensure_complete(trace: &LoadedTrace) -> Result<(), ReplayError> {
    if trace.completeness == TraceCompleteness::Complete && trace.completed.is_some() {
        Ok(())
    } else {
        Err(ReplayError::IncompleteTrace)
    }
}

pub(crate) fn adapter_from_metadata(
    metadata: &ProviderMetadata,
) -> Result<ProtocolAdapter, ReplayError> {
    if metadata.adapter != OPENAI_COMPATIBLE_ADAPTER
        || metadata.adapter_version != OPENAI_COMPATIBLE_ADAPTER_VERSION
    {
        return Err(ReplayError::UnsupportedAdapter {
            adapter: metadata.adapter.clone(),
            version: metadata.adapter_version,
        });
    }
    if metadata.protocol != OPENAI_CHAT_COMPLETIONS_PROTOCOL {
        return Err(ReplayError::InvalidMetadata(
            "protocol does not match adapter",
        ));
    }
    match metadata.protocol_adapter.as_str() {
        "generic" => {
            let provider = ProviderId::new(metadata.provider_id.clone())
                .map_err(|_| ReplayError::InvalidMetadata("provider id is invalid"))?;
            Ok(ProtocolAdapter::openai_compatible(provider))
        }
        "deepseek" if metadata.provider_id == "deepseek" => Ok(ProtocolAdapter::deepseek()),
        "deepseek" => Err(ReplayError::InvalidMetadata(
            "DeepSeek protocol_adapter requires the DeepSeek provider id",
        )),
        other => Err(ReplayError::UnsupportedProtocolAdapter(other.to_owned())),
    }
}

pub(crate) fn capabilities_from_adapter(protocol_adapter: &ProtocolAdapter) -> ModelCapabilities {
    ModelCapabilities {
        reasoning: protocol_adapter.supports_reasoning(),
        image_input: false,
        tool_calls: true,
        streaming: true,
    }
}

pub(crate) fn request_mismatch_field(
    expected: &ModelRequest,
    actual: &ModelRequest,
) -> Option<&'static str> {
    if expected.system != actual.system {
        Some("system")
    } else if expected.conversation != actual.conversation {
        Some("conversation")
    } else if expected.tools != actual.tools {
        Some("tools")
    } else if expected.tool_choice != actual.tool_choice {
        Some("tool_choice")
    } else if expected.generation != actual.generation {
        Some("generation")
    } else if expected.reasoning != actual.reasoning {
        Some("reasoning")
    } else if expected.provider_options != actual.provider_options {
        Some("provider_options")
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use agent_model::{
        GenerationConfig, ModelEvent, ModelRequest, ProviderOptions, SystemPromptSnapshot,
    };
    use agent_types::{
        AssistantMessage, AssistantPart, ConversationSnapshot, FinishReason, MessageId,
        ModelIdentity, PartId, ProviderId, TextPart, ToolChoice,
    };

    use super::*;
    use crate::trace::{
        LoadedTrace, ModelCallEvent, NativeTracePayload, ProviderMetadata, TraceCompleted,
        TraceLayer, TraceRecord, TraceStarted,
    };

    pub(crate) const CORRELATION: &str = "call-1";

    pub(crate) fn metadata() -> ProviderMetadata {
        ProviderMetadata {
            adapter: OPENAI_COMPATIBLE_ADAPTER.into(),
            adapter_version: OPENAI_COMPATIBLE_ADAPTER_VERSION,
            protocol_adapter: "generic".into(),
            provider_id: "fixture".into(),
            protocol: OPENAI_CHAT_COMPLETIONS_PROTOCOL.into(),
            endpoint: "https://example.invalid/v1".into(),
            model: "fixture-model".into(),
            context_window_tokens: 4096,
        }
    }

    pub(crate) fn request() -> ModelRequest {
        ModelRequest {
            system: SystemPromptSnapshot::new(vec!["system fixture".into()]),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }

    pub(crate) fn text_events() -> Vec<ModelEvent> {
        let message_id = MessageId::new("chatcmpl-2").unwrap();
        let part_id = PartId::new("part_1").unwrap();
        let model = ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture-model");
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id.clone(),
                model: model.clone(),
            },
            ModelEvent::TextStarted {
                id: part_id.clone(),
            },
            ModelEvent::TextDelta {
                id: part_id.clone(),
                delta: "Hello, ".into(),
            },
            ModelEvent::TextDelta {
                id: part_id.clone(),
                delta: "world!".into(),
            },
            ModelEvent::TextFinished {
                id: part_id.clone(),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id,
                    model,
                    parts: vec![AssistantPart::Text(TextPart {
                        id: part_id,
                        text: "Hello, world!".into(),
                    })],
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            },
        ]
    }

    pub(crate) fn model_trace(
        outcome: RecordedModelOutcome,
        events: Vec<ModelEvent>,
    ) -> LoadedTrace {
        let mut records = vec![TraceRecord {
            sequence: 1,
            observed_at_ms: 2,
            layer: TraceLayer::Model,
            correlation_id: Some(CORRELATION.into()),
            attempt: None,
            payload: NativeTracePayload::ModelRequest(request()),
        }];
        records.push(TraceRecord {
            sequence: 2,
            observed_at_ms: 3,
            layer: TraceLayer::Model,
            correlation_id: Some(CORRELATION.into()),
            attempt: None,
            payload: NativeTracePayload::ModelCall(match outcome {
                RecordedModelOutcome::Established => ModelCallEvent::StreamEstablished,
                RecordedModelOutcome::Failed(error) => {
                    ModelCallEvent::EstablishmentFailed { error }
                }
            }),
        });
        for event in events {
            let sequence = u64::try_from(records.len() + 1).unwrap();
            records.push(TraceRecord {
                sequence,
                observed_at_ms: sequence + 1,
                layer: TraceLayer::Model,
                correlation_id: Some(CORRELATION.into()),
                attempt: None,
                payload: NativeTracePayload::ModelEvent(event),
            });
        }
        let count = u64::try_from(records.len()).unwrap();
        LoadedTrace {
            started: TraceStarted {
                format_version: crate::trace::TRACE_FORMAT_VERSION,
                trace_id: "fixture-trace".into(),
                provider: metadata(),
                started_at_ms: 1,
            },
            records,
            completed: Some(TraceCompleted {
                last_sequence: count,
                record_count: count,
                completed_at_ms: count + 2,
            }),
            completeness: TraceCompleteness::Complete,
        }
    }
}
