//! 让录制的 HTTP/SSE wire 事实重新经过真实 Provider Decoder 的离线回放。

use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

use agent_model::{ModelAttemptEvent, ModelCallContext, ModelEvent, ModelService, TraceContext};
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, ProviderWireEvent, RecordedWireRequest, Transport,
    TransportError, TransportFuture, TransportRequest, TransportResponse,
};
use futures_util::{StreamExt, stream};
use tokio_util::sync::CancellationToken;

use crate::{
    replay::{RecordedModelOutcome, ReplayError, profile_from_metadata, recorded_model_calls},
    trace::{LoadedTrace, NativeTracePayload},
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WireKey {
    correlation_id: String,
    attempt: Option<u32>,
}

#[derive(Clone)]
enum WireResponse {
    EstablishmentFailed(TransportError),
    Started {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<Result<Vec<u8>, TransportError>>,
    },
}

#[derive(Clone)]
struct WireExchange {
    first_sequence: u64,
    request: RecordedWireRequest,
    response: WireResponse,
}

#[derive(Default)]
struct WireExchangeBuilder {
    first_sequence: Option<u64>,
    request: Option<RecordedWireRequest>,
    response_started: Option<(u16, Vec<(String, String)>)>,
    establishment_error: Option<TransportError>,
    body: Vec<Result<Vec<u8>, TransportError>>,
    terminal: bool,
}

#[derive(Clone)]
pub(crate) struct ReplayTransport {
    scripts: Arc<Mutex<BTreeMap<WireKey, WireExchange>>>,
    mismatch: Arc<Mutex<Option<ReplayError>>>,
}

impl ReplayTransport {
    fn new(scripts: BTreeMap<WireKey, WireExchange>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(scripts)),
            mismatch: Arc::new(Mutex::new(None)),
        }
    }

    fn set_mismatch(&self, error: ReplayError) {
        let mut mismatch = self
            .mismatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if mismatch.is_none() {
            *mismatch = Some(error);
        }
    }

    fn take_mismatch(&self) -> Option<ReplayError> {
        self.mismatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn remaining(&self) -> usize {
        self.scripts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Transport for ReplayTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        Box::pin(async move {
            let Some(trace) = &request.trace else {
                self.set_mismatch(ReplayError::RequestMismatch {
                    layer: "wire",
                    field: "trace",
                });
                return Err(TransportError::Connect(
                    "wire replay request is missing trace context".into(),
                ));
            };
            let key = WireKey {
                correlation_id: trace.correlation_id.clone(),
                attempt: trace.attempt.map(|attempt| attempt.get()),
            };
            let exchange = self
                .scripts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
            let Some(exchange) = exchange else {
                self.set_mismatch(ReplayError::ScriptExhausted("wire"));
                return Err(TransportError::Connect(
                    "wire replay script does not contain this attempt".into(),
                ));
            };
            let actual = RecordedWireRequest::from_transport_request(&request);
            if let Some(field) = wire_request_mismatch_field(&exchange.request, &actual) {
                self.set_mismatch(ReplayError::RequestMismatch {
                    layer: "wire",
                    field,
                });
                return Err(TransportError::Connect(format!(
                    "wire replay request mismatch in `{field}`"
                )));
            }
            match exchange.response {
                WireResponse::EstablishmentFailed(error) => Err(error),
                WireResponse::Started {
                    status,
                    headers,
                    body,
                } => Ok(TransportResponse {
                    status,
                    headers,
                    body: Box::pin(stream::iter(body)),
                }),
            }
        })
    }
}

struct WireReplayCase {
    first_sequence: u64,
    key: WireKey,
    request: agent_model::ModelRequest,
    expected_outcome: RecordedModelOutcome,
    expected_events: Vec<ModelEvent>,
}

/// CLI 逐 attempt 重放 wire 输入，并比较 Decoder 产生的规范边界结果。
pub(crate) async fn run_wire_replay(trace: &LoadedTrace) -> Result<usize, ReplayError> {
    let calls = recorded_model_calls(trace)?;
    let call_by_id = calls
        .iter()
        .map(|call| (call.correlation_id.clone(), call))
        .collect::<BTreeMap<_, _>>();
    let attempt_outcomes = attempt_outcomes(trace)?;
    let exchanges = wire_exchanges(trace)?;
    for call in &calls {
        if !exchanges
            .keys()
            .any(|key| key.correlation_id == call.correlation_id)
        {
            return Err(ReplayError::CorruptScript(format!(
                "model correlation {} has no wire exchange",
                call.correlation_id
            )));
        }
    }
    for key in attempt_outcomes.keys() {
        if !exchanges.contains_key(key) {
            return Err(ReplayError::CorruptScript(format!(
                "model correlation {} has an attempt outcome without wire facts",
                key.correlation_id
            )));
        }
    }
    let mut cases = exchanges
        .iter()
        .map(|(key, exchange)| {
            let call = call_by_id.get(&key.correlation_id).ok_or_else(|| {
                ReplayError::CorruptScript(format!(
                    "wire correlation {} has no model call",
                    key.correlation_id
                ))
            })?;
            let expected_outcome = if key.attempt.is_some() {
                attempt_outcomes.get(key).cloned().ok_or_else(|| {
                    ReplayError::CorruptScript(format!(
                        "wire correlation {} has no attempt outcome",
                        key.correlation_id
                    ))
                })?
            } else {
                call.outcome.clone()
            };
            let expected_events = if expected_outcome == RecordedModelOutcome::Established {
                call.events.clone()
            } else {
                vec![]
            };
            Ok(WireReplayCase {
                first_sequence: exchange.first_sequence,
                key: key.clone(),
                request: call.request.clone(),
                expected_outcome,
                expected_events,
            })
        })
        .collect::<Result<Vec<_>, ReplayError>>()?;
    cases.sort_by_key(|case| case.first_sequence);

    let replay_transport = Arc::new(ReplayTransport::new(exchanges));
    let profile = profile_from_metadata(&trace.started.provider)?;
    // credential 只用于 Service 的既有构造契约；安全请求比较会永久删除该 header，
    // ReplayTransport 是唯一 Transport，因此这里没有网络客户端或真实认证数据。
    let service = OpenAiCompatibleService::with_transport(
        trace.started.provider.endpoint.clone(),
        BearerCredential::new("replay-placeholder"),
        trace.started.provider.model.clone(),
        trace.started.provider.context_window_tokens,
        profile,
        replay_transport.clone(),
    )
    .map_err(|error| ReplayError::Service(error.to_string()))?;

    for case in &cases {
        let trace_context = match case.key.attempt {
            Some(attempt) => TraceContext::new(case.key.correlation_id.clone()).with_attempt(
                NonZeroU32::new(attempt).ok_or_else(|| {
                    ReplayError::CorruptScript("wire attempt must be 1-based".into())
                })?,
            ),
            None => TraceContext::new(case.key.correlation_id.clone()),
        };
        let result = service
            .stream(
                case.request.clone(),
                ModelCallContext {
                    cancellation: CancellationToken::new(),
                    trace: Some(trace_context),
                },
            )
            .await;
        if let Some(error) = replay_transport.take_mismatch() {
            return Err(error);
        }
        match (&case.expected_outcome, result) {
            (RecordedModelOutcome::Failed(expected), Err(actual)) if expected == &actual => {}
            (RecordedModelOutcome::Established, Ok(mut stream)) => {
                let mut actual = Vec::new();
                while let Some(event) = stream.next().await {
                    actual.push(event);
                }
                if actual != case.expected_events {
                    return Err(ReplayError::ResultMismatch {
                        layer: "wire",
                        correlation_id: case.key.correlation_id.clone(),
                    });
                }
            }
            _ => {
                return Err(ReplayError::ResultMismatch {
                    layer: "wire",
                    correlation_id: case.key.correlation_id.clone(),
                });
            }
        }
    }
    if replay_transport.remaining() != 0 {
        return Err(ReplayError::CorruptScript(
            "wire replay did not consume every attempt".into(),
        ));
    }
    Ok(cases.len())
}

fn wire_exchanges(trace: &LoadedTrace) -> Result<BTreeMap<WireKey, WireExchange>, ReplayError> {
    let mut builders = BTreeMap::<WireKey, WireExchangeBuilder>::new();
    for record in &trace.records {
        let NativeTracePayload::ProviderWire(event) = &record.payload else {
            continue;
        };
        let key = WireKey {
            correlation_id: record.correlation_id.clone().ok_or_else(|| {
                ReplayError::CorruptScript("wire record has no correlation id".into())
            })?,
            attempt: record.attempt,
        };
        let builder = builders.entry(key.clone()).or_default();
        if builder.terminal {
            return Err(ReplayError::CorruptScript(format!(
                "wire attempt {} has events after its terminal",
                key.correlation_id
            )));
        }
        builder.first_sequence.get_or_insert(record.sequence);
        match event {
            ProviderWireEvent::Request { request, .. } => {
                if builder.request.replace(request.clone()).is_some() {
                    return Err(ReplayError::CorruptScript(format!(
                        "wire attempt {} has duplicate requests",
                        key.correlation_id
                    )));
                }
            }
            ProviderWireEvent::ResponseStarted {
                status, headers, ..
            } => {
                if builder
                    .response_started
                    .replace((*status, headers.clone()))
                    .is_some()
                {
                    return Err(ReplayError::CorruptScript(format!(
                        "wire attempt {} has duplicate response starts",
                        key.correlation_id
                    )));
                }
            }
            ProviderWireEvent::ResponseChunk { bytes, .. } => {
                if builder.response_started.is_none() {
                    return Err(ReplayError::CorruptScript(
                        "wire response chunk arrived before response start".into(),
                    ));
                }
                builder.body.push(Ok(bytes.clone()));
            }
            ProviderWireEvent::ResponseFailed { error, .. } => {
                if builder.response_started.is_some() {
                    builder.body.push(Err(error.clone()));
                } else {
                    builder.establishment_error = Some(error.clone());
                }
                builder.terminal = true;
            }
            ProviderWireEvent::ResponseFinished { .. } => {
                if builder.response_started.is_none() {
                    return Err(ReplayError::CorruptScript(
                        "wire response finished before response start".into(),
                    ));
                }
                builder.terminal = true;
            }
        }
    }

    builders
        .into_iter()
        .map(|(key, builder)| {
            let first_sequence = builder
                .first_sequence
                .ok_or_else(|| ReplayError::CorruptScript("wire attempt has no sequence".into()))?;
            let request = builder
                .request
                .ok_or_else(|| ReplayError::CorruptScript("wire attempt has no request".into()))?;
            let response = match (builder.establishment_error, builder.response_started) {
                (Some(error), None) => WireResponse::EstablishmentFailed(error),
                (None, Some((status, headers))) => WireResponse::Started {
                    status,
                    headers,
                    body: builder.body,
                },
                _ => {
                    return Err(ReplayError::CorruptScript(
                        "wire attempt has an ambiguous response boundary".into(),
                    ));
                }
            };
            Ok((
                key,
                WireExchange {
                    first_sequence,
                    request,
                    response,
                },
            ))
        })
        .collect()
}

fn attempt_outcomes(
    trace: &LoadedTrace,
) -> Result<BTreeMap<WireKey, RecordedModelOutcome>, ReplayError> {
    let mut outcomes = BTreeMap::new();
    for record in &trace.records {
        let NativeTracePayload::ModelAttempt(event) = &record.payload else {
            continue;
        };
        let outcome = match event {
            ModelAttemptEvent::EstablishmentFailed { error, .. } => {
                Some(RecordedModelOutcome::Failed(error.clone()))
            }
            ModelAttemptEvent::StreamEstablished { .. } => Some(RecordedModelOutcome::Established),
            _ => None,
        };
        let Some(outcome) = outcome else {
            continue;
        };
        let key = WireKey {
            correlation_id: record.correlation_id.clone().ok_or_else(|| {
                ReplayError::CorruptScript("attempt outcome has no correlation id".into())
            })?,
            attempt: record.attempt,
        };
        if outcomes.insert(key, outcome).is_some() {
            return Err(ReplayError::CorruptScript(
                "attempt has duplicate establishment outcomes".into(),
            ));
        }
    }
    Ok(outcomes)
}

fn wire_request_mismatch_field(
    expected: &RecordedWireRequest,
    actual: &RecordedWireRequest,
) -> Option<&'static str> {
    if expected.method != actual.method {
        Some("method")
    } else if expected.url != actual.url {
        Some("url")
    } else if expected.headers != actual.headers {
        Some("headers")
    } else if expected.body != actual.body {
        Some("body")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use agent_model::{ModelError, ModelRetryReason, ModelTransportErrorKind};
    use agent_provider_openai_compatible::{Profile, encode_request};
    use agent_types::ProviderId;

    use super::*;
    use crate::{
        replay::{
            RecordedModelOutcome, ReplayError,
            test_support::{CORRELATION, metadata, model_trace, request, text_events},
        },
        trace::{NativeTracePayload, TraceLayer, TraceRecord},
    };

    const FRAME_HELLO: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello, \"},\"finish_reason\":null}]}\n\n";
    const FRAME_WORLD: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n";
    const FRAME_FINISH: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    const FRAME_DONE: &str = "data: [DONE]\n\n";

    fn recorded_request() -> RecordedWireRequest {
        let metadata = metadata();
        let profile = Profile::openai_compatible(ProviderId::new("fixture").unwrap());
        let encoded = encode_request(&request(), &profile, &metadata.model).unwrap();
        RecordedWireRequest {
            method: "POST".into(),
            url: format!("{}/chat/completions", metadata.endpoint),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
            ],
            body: serde_json::to_vec(&encoded).unwrap(),
        }
    }

    fn with_wire_events(mut trace: LoadedTrace, events: Vec<ProviderWireEvent>) -> LoadedTrace {
        for event in events {
            let sequence = u64::try_from(trace.records.len() + 1).unwrap();
            trace.records.push(TraceRecord {
                sequence,
                observed_at_ms: sequence + 1,
                layer: TraceLayer::Provider,
                correlation_id: Some(CORRELATION.into()),
                attempt: None,
                payload: NativeTracePayload::ProviderWire(event),
            });
        }
        let count = u64::try_from(trace.records.len()).unwrap();
        let completed = trace.completed.as_mut().unwrap();
        completed.last_sequence = count;
        completed.record_count = count;
        trace
    }

    fn push_record(
        trace: &mut LoadedTrace,
        trace_context: &TraceContext,
        payload: NativeTracePayload,
    ) {
        let sequence = u64::try_from(trace.records.len() + 1).unwrap();
        trace.records.push(TraceRecord {
            sequence,
            observed_at_ms: sequence + 1,
            layer: payload.layer(),
            correlation_id: Some(trace_context.correlation_id.clone()),
            attempt: trace_context.attempt.map(|attempt| attempt.get()),
            payload,
        });
        let count = u64::try_from(trace.records.len()).unwrap();
        let completed = trace.completed.as_mut().unwrap();
        completed.last_sequence = count;
        completed.record_count = count;
    }

    fn successful_wire_events(chunks: Vec<Vec<u8>>) -> Vec<ProviderWireEvent> {
        let trace = Some(TraceContext::new(CORRELATION));
        let mut events = vec![
            ProviderWireEvent::Request {
                trace: trace.clone(),
                request: recorded_request(),
            },
            ProviderWireEvent::ResponseStarted {
                trace: trace.clone(),
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
            },
        ];
        events.extend(
            chunks
                .into_iter()
                .map(|bytes| ProviderWireEvent::ResponseChunk {
                    trace: trace.clone(),
                    bytes,
                }),
        );
        events.push(ProviderWireEvent::ResponseFinished { trace });
        events
    }

    fn with_trace(event: ProviderWireEvent, trace: &TraceContext) -> ProviderWireEvent {
        let trace = Some(trace.clone());
        match event {
            ProviderWireEvent::Request { request, .. } => {
                ProviderWireEvent::Request { trace, request }
            }
            ProviderWireEvent::ResponseStarted {
                status, headers, ..
            } => ProviderWireEvent::ResponseStarted {
                trace,
                status,
                headers,
            },
            ProviderWireEvent::ResponseChunk { bytes, .. } => {
                ProviderWireEvent::ResponseChunk { trace, bytes }
            }
            ProviderWireEvent::ResponseFailed { error, .. } => {
                ProviderWireEvent::ResponseFailed { trace, error }
            }
            ProviderWireEvent::ResponseFinished { .. } => {
                ProviderWireEvent::ResponseFinished { trace }
            }
        }
    }

    #[tokio::test]
    async fn wire_replay_redecodes_success_across_recorded_chunk_boundaries() {
        let body = format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}{FRAME_DONE}");
        for chunks in [
            vec![body.as_bytes().to_vec()],
            body.as_bytes()
                .chunks(7)
                .map(<[u8]>::to_vec)
                .collect::<Vec<_>>(),
        ] {
            let trace = with_wire_events(
                model_trace(RecordedModelOutcome::Established, text_events()),
                successful_wire_events(chunks),
            );
            assert_eq!(run_wire_replay(&trace).await.unwrap(), 1);
        }
    }

    #[tokio::test]
    async fn wire_replay_selects_each_attempt_by_correlation_and_attempt() {
        let mut trace = model_trace(RecordedModelOutcome::Established, text_events());
        let attempt_1 = TraceContext::new(CORRELATION).with_attempt(NonZeroU32::new(1).unwrap());
        let attempt_2 = TraceContext::new(CORRELATION).with_attempt(NonZeroU32::new(2).unwrap());
        let first_error = ModelError::Transport {
            kind: ModelTransportErrorKind::Connection,
            message: "first attempt refused".into(),
        };

        push_record(
            &mut trace,
            &attempt_1,
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::Started {
                trace: Some(attempt_1.clone()),
                attempt: 1,
            }),
        );
        push_record(
            &mut trace,
            &attempt_1,
            NativeTracePayload::ProviderWire(ProviderWireEvent::Request {
                trace: Some(attempt_1.clone()),
                request: recorded_request(),
            }),
        );
        push_record(
            &mut trace,
            &attempt_1,
            NativeTracePayload::ProviderWire(ProviderWireEvent::ResponseFailed {
                trace: Some(attempt_1.clone()),
                error: TransportError::Connect("first attempt refused".into()),
            }),
        );
        push_record(
            &mut trace,
            &attempt_1,
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::EstablishmentFailed {
                trace: Some(attempt_1.clone()),
                attempt: 1,
                error: first_error,
                retry_reason: Some(ModelRetryReason::Connection),
                will_retry: true,
            }),
        );
        push_record(
            &mut trace,
            &attempt_2,
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::Started {
                trace: Some(attempt_2.clone()),
                attempt: 2,
            }),
        );
        let body = format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}{FRAME_DONE}");
        for event in successful_wire_events(vec![body.into_bytes()]) {
            let event = with_trace(event, &attempt_2);
            push_record(
                &mut trace,
                &attempt_2,
                NativeTracePayload::ProviderWire(event),
            );
        }
        push_record(
            &mut trace,
            &attempt_2,
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::StreamEstablished {
                trace: Some(attempt_2.clone()),
                attempt: 2,
            }),
        );

        assert_eq!(run_wire_replay(&trace).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn wire_replay_preserves_establishment_non_2xx_and_stream_errors() {
        let connect_error = TransportError::Connect("fixture refused".into());
        let model_error = ModelError::Transport {
            kind: ModelTransportErrorKind::Connection,
            message: "fixture refused".into(),
        };
        let trace_context = Some(TraceContext::new(CORRELATION));
        let connection = with_wire_events(
            model_trace(RecordedModelOutcome::Failed(model_error), vec![]),
            vec![
                ProviderWireEvent::Request {
                    trace: trace_context.clone(),
                    request: recorded_request(),
                },
                ProviderWireEvent::ResponseFailed {
                    trace: trace_context.clone(),
                    error: connect_error,
                },
            ],
        );
        assert_eq!(run_wire_replay(&connection).await.unwrap(), 1);

        let unavailable = ModelError::Unavailable {
            message: "busy".into(),
            status: Some(503),
            retry_after_ms: Some(2_000),
        };
        let error_body = br#"{"error":{"message":"busy","type":"server_error","code":null}}"#;
        let non_2xx = with_wire_events(
            model_trace(RecordedModelOutcome::Failed(unavailable), vec![]),
            vec![
                ProviderWireEvent::Request {
                    trace: trace_context.clone(),
                    request: recorded_request(),
                },
                ProviderWireEvent::ResponseStarted {
                    trace: trace_context.clone(),
                    status: 503,
                    headers: vec![("retry-after".into(), "2".into())],
                },
                ProviderWireEvent::ResponseChunk {
                    trace: trace_context.clone(),
                    bytes: error_body.to_vec(),
                },
                ProviderWireEvent::ResponseFinished {
                    trace: trace_context.clone(),
                },
            ],
        );
        assert_eq!(run_wire_replay(&non_2xx).await.unwrap(), 1);

        let interrupted_error = ModelError::Transport {
            kind: ModelTransportErrorKind::Interrupted,
            message: "fixture reset".into(),
        };
        let interrupted_events = vec![
            text_events()[0].clone(),
            text_events()[1].clone(),
            text_events()[2].clone(),
            ModelEvent::TurnFailed {
                error: interrupted_error,
            },
        ];
        let interrupted = with_wire_events(
            model_trace(RecordedModelOutcome::Established, interrupted_events),
            vec![
                ProviderWireEvent::Request {
                    trace: trace_context.clone(),
                    request: recorded_request(),
                },
                ProviderWireEvent::ResponseStarted {
                    trace: trace_context.clone(),
                    status: 200,
                    headers: vec![("content-type".into(), "text/event-stream".into())],
                },
                ProviderWireEvent::ResponseChunk {
                    trace: trace_context.clone(),
                    bytes: FRAME_HELLO.as_bytes().to_vec(),
                },
                ProviderWireEvent::ResponseFailed {
                    trace: trace_context,
                    error: TransportError::Interrupted("fixture reset".into()),
                },
            ],
        );
        assert_eq!(run_wire_replay(&interrupted).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn wire_replay_reports_request_adapter_profile_and_script_mismatch() {
        let body = format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}{FRAME_DONE}");
        let mut request_mismatch = with_wire_events(
            model_trace(RecordedModelOutcome::Established, text_events()),
            successful_wire_events(vec![body.into_bytes()]),
        );
        let NativeTracePayload::ProviderWire(ProviderWireEvent::Request { request, .. }) =
            &mut request_mismatch.records[8].payload
        else {
            panic!("expected wire request")
        };
        request.body.push(0);
        assert!(matches!(
            run_wire_replay(&request_mismatch).await,
            Err(ReplayError::RequestMismatch {
                layer: "wire",
                field: "body"
            })
        ));

        let mut adapter = request_mismatch.clone();
        adapter.started.provider.adapter_version += 1;
        assert!(matches!(
            run_wire_replay(&adapter).await,
            Err(ReplayError::UnsupportedAdapter { .. })
        ));

        let mut profile = request_mismatch;
        profile.started.provider.profile = "unknown".into();
        assert!(matches!(
            run_wire_replay(&profile).await,
            Err(ReplayError::UnsupportedProfile(_))
        ));

        let corrupt = with_wire_events(
            model_trace(RecordedModelOutcome::Established, text_events()),
            vec![ProviderWireEvent::ResponseChunk {
                trace: Some(TraceContext::new(CORRELATION)),
                bytes: b"data: [DONE]\n\n".to_vec(),
            }],
        );
        assert!(matches!(
            run_wire_replay(&corrupt).await,
            Err(ReplayError::CorruptScript(_))
        ));
    }

    #[tokio::test]
    async fn wire_replay_honors_pre_cancel_without_consuming_attempt() {
        let body = format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}{FRAME_DONE}");
        let trace = with_wire_events(
            model_trace(RecordedModelOutcome::Established, text_events()),
            successful_wire_events(vec![body.into_bytes()]),
        );
        let exchanges = wire_exchanges(&trace).unwrap();
        let transport = Arc::new(ReplayTransport::new(exchanges));
        let service = OpenAiCompatibleService::with_transport(
            trace.started.provider.endpoint.clone(),
            BearerCredential::new("placeholder"),
            trace.started.provider.model.clone(),
            trace.started.provider.context_window_tokens,
            profile_from_metadata(&trace.started.provider).unwrap(),
            transport.clone(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let Err(error) = service
            .stream(
                request(),
                ModelCallContext {
                    cancellation,
                    trace: Some(TraceContext::new(CORRELATION)),
                },
            )
            .await
        else {
            panic!("pre-cancelled replay unexpectedly established a stream")
        };
        assert_eq!(error, ModelError::Cancelled);
        assert_eq!(transport.remaining(), 1);
    }
}
