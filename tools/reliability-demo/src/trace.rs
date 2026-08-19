//! Reliability Demo 私有的版本化 Trace 文件类型与严格 Loader。

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use agent_core::{AgentEvent, ExecutionOutcome};
use agent_model::{ModelAttemptEvent, ModelError, ModelEvent, ModelRequest, TraceContext};
use agent_openai_compatible::ProviderWireEvent;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 当前 Demo 私有 JSONL 格式版本。
pub(crate) const TRACE_FORMAT_VERSION: u32 = 1;

/// 重建 Provider Decoder 所需的非认证元数据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProviderMetadata {
    pub(crate) adapter: String,
    pub(crate) adapter_version: u32,
    #[serde(alias = "profile")]
    pub(crate) protocol_adapter: String,
    pub(crate) provider_id: String,
    pub(crate) protocol: String,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) context_window_tokens: u64,
}

/// ModelService 建流边界在 Demo 中缺失的最小宿主观察事实。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum ModelCallEvent {
    StreamEstablished,
    EstablishmentFailed { error: ModelError },
}

/// Trace 中原生事实所属的层级。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceLayer {
    Provider,
    Model,
    Agent,
    Host,
}

impl TraceLayer {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Agent => "agent",
            Self::Host => "host",
        }
    }
}

/// Demo 为验证录制闭环所需的少量宿主事实。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum DemoHostEvent {
    AgentExecutionStarted,
    AgentExecutionFinished { outcome: ExecutionOutcome },
    CancellationRequested,
    JournalBeginFinished { succeeded: bool },
    JournalCommitFinished { succeeded: bool },
}

/// Trace 直接包裹各层已经定义的原生事实，不复制其字段。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum NativeTracePayload {
    ModelRequest(ModelRequest),
    ModelCall(ModelCallEvent),
    ModelEvent(ModelEvent),
    ModelAttempt(ModelAttemptEvent),
    ProviderWire(ProviderWireEvent),
    Agent(AgentEvent),
    Host(DemoHostEvent),
}

impl NativeTracePayload {
    pub(crate) fn layer(&self) -> TraceLayer {
        match self {
            Self::ProviderWire(_) => TraceLayer::Provider,
            Self::ModelRequest(_)
            | Self::ModelCall(_)
            | Self::ModelEvent(_)
            | Self::ModelAttempt(_) => TraceLayer::Model,
            Self::Agent(_) => TraceLayer::Agent,
            Self::Host(_) => TraceLayer::Host,
        }
    }

    /// attempt/wire 事实天然携带 TraceContext；其他层由观察宿主显式关联。
    pub(crate) fn native_trace(&self) -> Option<&TraceContext> {
        match self {
            Self::ModelAttempt(event) => attempt_event_trace(event),
            Self::ProviderWire(event) => wire_event_trace(event),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceStarted {
    pub(crate) format_version: u32,
    pub(crate) trace_id: String,
    pub(crate) provider: ProviderMetadata,
    pub(crate) started_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceRecord {
    pub(crate) sequence: u64,
    pub(crate) observed_at_ms: u64,
    pub(crate) layer: TraceLayer,
    pub(crate) correlation_id: Option<String>,
    pub(crate) attempt: Option<u32>,
    pub(crate) payload: NativeTracePayload,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceCompleted {
    pub(crate) last_sequence: u64,
    pub(crate) record_count: u64,
    pub(crate) completed_at_ms: u64,
}

/// 每行独立带 tag，断行、重复头尾或未知形状都能明确拒绝。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub(crate) enum TraceLine {
    Started(TraceStarted),
    Record(TraceRecord),
    Completed(TraceCompleted),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceCompleteness {
    Complete,
    Incomplete,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoadedTrace {
    pub(crate) started: TraceStarted,
    pub(crate) records: Vec<TraceRecord>,
    pub(crate) completed: Option<TraceCompleted>,
    pub(crate) completeness: TraceCompleteness,
}

#[derive(Debug, Error)]
pub(crate) enum TraceLoadError {
    #[error("failed to read trace: {0}")]
    Read(#[source] std::io::Error),
    #[error("trace is not valid UTF-8")]
    Encoding,
    #[error("trace line {line} is invalid JSON: {source}")]
    Json {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("trace is empty")]
    Empty,
    #[error("trace must start with exactly one started line")]
    InvalidStart,
    #[error("unsupported trace format version {0}")]
    UnsupportedVersion(u32),
    #[error("trace file name does not match trace id")]
    TraceIdMismatch,
    #[error("trace contains a record after its completed line")]
    RecordAfterCompletion,
    #[error("trace sequence mismatch: expected {expected}, got {actual}")]
    SequenceMismatch { expected: u64, actual: u64 },
    #[error("trace completed count does not match its records")]
    CountMismatch,
    #[error("complete trace is missing its completed line")]
    MissingCompletion,
    #[error("incomplete trace cannot be used for exact replay")]
    Incomplete,
    #[error("trace correlation is invalid: {0}")]
    InvalidCorrelation(String),
    #[error("trace model lifecycle is invalid: {0}")]
    InvalidModelLifecycle(String),
}

/// Timeline 可以读取结构完整但没有完成尾的 `.incomplete.jsonl`。
pub(crate) async fn load_for_timeline(path: &Path) -> Result<LoadedTrace, TraceLoadError> {
    load(path, false).await
}

/// 精确回放入口只接受带完成尾、通过全部关联校验的 Complete Trace。
pub(crate) async fn load_complete(path: &Path) -> Result<LoadedTrace, TraceLoadError> {
    load(path, true).await
}

async fn load(path: &Path, require_complete: bool) -> Result<LoadedTrace, TraceLoadError> {
    let bytes = tokio::fs::read(path).await.map_err(TraceLoadError::Read)?;
    let source = std::str::from_utf8(&bytes).map_err(|_| TraceLoadError::Encoding)?;
    let mut lines = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if line.is_empty() {
            return Err(TraceLoadError::Json {
                line: index + 1,
                source: serde_json::from_str::<TraceLine>(line).unwrap_err(),
            });
        }
        lines.push(serde_json::from_str::<TraceLine>(line).map_err(|source| {
            TraceLoadError::Json {
                line: index + 1,
                source,
            }
        })?);
    }

    let first = lines.first().ok_or(TraceLoadError::Empty)?;
    let TraceLine::Started(started) = first else {
        return Err(TraceLoadError::InvalidStart);
    };
    let started = started.clone();
    if started.format_version != TRACE_FORMAT_VERSION {
        return Err(TraceLoadError::UnsupportedVersion(started.format_version));
    }
    validate_trace_id(path, &started.trace_id)?;

    let mut records = Vec::new();
    let mut completed = None;
    let mut expected_sequence = 1_u64;
    for line in lines.into_iter().skip(1) {
        match line {
            TraceLine::Started(_) => return Err(TraceLoadError::InvalidStart),
            TraceLine::Record(record) => {
                if completed.is_some() {
                    return Err(TraceLoadError::RecordAfterCompletion);
                }
                if record.sequence != expected_sequence {
                    return Err(TraceLoadError::SequenceMismatch {
                        expected: expected_sequence,
                        actual: record.sequence,
                    });
                }
                validate_record_envelope(&record)?;
                expected_sequence += 1;
                records.push(record);
            }
            TraceLine::Completed(tail) => {
                if completed.replace(tail).is_some() {
                    return Err(TraceLoadError::RecordAfterCompletion);
                }
            }
        }
    }

    let completeness = if let Some(tail) = &completed {
        let actual_count = u64::try_from(records.len()).unwrap_or(u64::MAX);
        let last_sequence = records.last().map_or(0, |record| record.sequence);
        if tail.record_count != actual_count || tail.last_sequence != last_sequence {
            return Err(TraceLoadError::CountMismatch);
        }
        validate_complete_calls(&records)?;
        TraceCompleteness::Complete
    } else {
        TraceCompleteness::Incomplete
    };

    match (completeness, is_incomplete_path(path), require_complete) {
        (TraceCompleteness::Complete, true, _) => return Err(TraceLoadError::TraceIdMismatch),
        (TraceCompleteness::Incomplete, false, _) => {
            return Err(TraceLoadError::MissingCompletion);
        }
        (TraceCompleteness::Incomplete, true, true) => return Err(TraceLoadError::Incomplete),
        _ => {}
    }

    Ok(LoadedTrace {
        started,
        records,
        completed,
        completeness,
    })
}

fn validate_trace_id(path: &Path, trace_id: &str) -> Result<(), TraceLoadError> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(TraceLoadError::TraceIdMismatch);
    };
    let expected = if let Some(id) = file_name.strip_suffix(".incomplete.jsonl") {
        id
    } else if let Some(id) = file_name.strip_suffix(".jsonl") {
        id
    } else {
        return Err(TraceLoadError::TraceIdMismatch);
    };
    if expected == trace_id {
        Ok(())
    } else {
        Err(TraceLoadError::TraceIdMismatch)
    }
}

fn is_incomplete_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".incomplete.jsonl"))
}

fn validate_record_envelope(record: &TraceRecord) -> Result<(), TraceLoadError> {
    if record.layer != record.payload.layer() {
        return Err(TraceLoadError::InvalidCorrelation(
            "payload layer does not match record layer".into(),
        ));
    }
    if record.attempt == Some(0) || (record.attempt.is_some() && record.correlation_id.is_none()) {
        return Err(TraceLoadError::InvalidCorrelation(
            "attempt must be positive and have a correlation id".into(),
        ));
    }
    if let Some(native_trace) = record.payload.native_trace()
        && (record.correlation_id.as_deref() != Some(native_trace.correlation_id.as_str())
            || record.attempt != native_trace.attempt.map(|attempt| attempt.get()))
    {
        return Err(TraceLoadError::InvalidCorrelation(
            "record envelope does not match native trace".into(),
        ));
    }
    if let NativeTracePayload::ModelAttempt(event) = &record.payload {
        let event_attempt = match event {
            ModelAttemptEvent::Started { attempt, .. }
            | ModelAttemptEvent::EstablishmentFailed { attempt, .. }
            | ModelAttemptEvent::StreamEstablished { attempt, .. } => *attempt,
            ModelAttemptEvent::RetryScheduled { next_attempt, .. } => *next_attempt,
        };
        if record.attempt != Some(event_attempt) {
            return Err(TraceLoadError::InvalidCorrelation(
                "attempt event does not match its trace".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Default)]
struct CallValidation {
    requests: u32,
    call_outcomes: u32,
    call_established: bool,
    call_failed: bool,
    model_events: u32,
    terminal_events: u32,
    terminal_seen: bool,
    next_started_attempt: u32,
    attempt_outcomes: BTreeSet<u32>,
    last_observed_attempt: u32,
    stream_established: bool,
}

fn validate_complete_calls(records: &[TraceRecord]) -> Result<(), TraceLoadError> {
    let mut calls = BTreeMap::<String, CallValidation>::new();
    for record in records {
        let payload_needs_call = matches!(
            record.payload,
            NativeTracePayload::ModelRequest(_)
                | NativeTracePayload::ModelCall(_)
                | NativeTracePayload::ModelEvent(_)
                | NativeTracePayload::ModelAttempt(_)
                | NativeTracePayload::ProviderWire(_)
        );
        if payload_needs_call && record.correlation_id.is_none() {
            return Err(TraceLoadError::InvalidCorrelation(
                "model and provider facts require a correlation id".into(),
            ));
        }
        if !payload_needs_call {
            continue;
        }
        let Some(correlation_id) = &record.correlation_id else {
            continue;
        };
        let call = calls.entry(correlation_id.clone()).or_default();
        if let Some(attempt) = record.attempt {
            if call.last_observed_attempt == 0 {
                if attempt != 1 {
                    return Err(TraceLoadError::InvalidCorrelation(format!(
                        "call {correlation_id} starts at attempt {attempt}"
                    )));
                }
            } else if attempt < call.last_observed_attempt
                || attempt > call.last_observed_attempt.saturating_add(1)
            {
                return Err(TraceLoadError::InvalidCorrelation(format!(
                    "call {correlation_id} has a non-contiguous attempt"
                )));
            }
            call.last_observed_attempt = attempt;
        }

        match &record.payload {
            NativeTracePayload::ModelRequest(_) => call.requests += 1,
            NativeTracePayload::ModelCall(outcome) => {
                call.call_outcomes += 1;
                match outcome {
                    ModelCallEvent::StreamEstablished => call.call_established = true,
                    ModelCallEvent::EstablishmentFailed { .. } => call.call_failed = true,
                }
            }
            NativeTracePayload::ModelEvent(event) => {
                if call.terminal_seen {
                    return Err(TraceLoadError::InvalidModelLifecycle(format!(
                        "call {correlation_id} has an event after its terminal"
                    )));
                }
                call.model_events += 1;
                if event.is_terminal() {
                    call.terminal_events += 1;
                    call.terminal_seen = true;
                }
            }
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::Started { attempt, .. }) => {
                let expected = call.next_started_attempt.saturating_add(1);
                if *attempt != expected {
                    return Err(TraceLoadError::InvalidCorrelation(format!(
                        "call {correlation_id} expected started attempt {expected}, got {attempt}"
                    )));
                }
                call.next_started_attempt = *attempt;
            }
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::EstablishmentFailed {
                attempt,
                ..
            }) => {
                if !call.attempt_outcomes.insert(*attempt) {
                    return Err(TraceLoadError::InvalidCorrelation(format!(
                        "call {correlation_id} has duplicate attempt outcomes"
                    )));
                }
            }
            NativeTracePayload::ModelAttempt(ModelAttemptEvent::StreamEstablished {
                attempt,
                ..
            }) => {
                if !call.attempt_outcomes.insert(*attempt) {
                    return Err(TraceLoadError::InvalidCorrelation(format!(
                        "call {correlation_id} has duplicate attempt outcomes"
                    )));
                }
                call.stream_established = true;
            }
            _ => {}
        }
    }

    for (correlation_id, call) in calls {
        if call.requests != 1 {
            return Err(TraceLoadError::InvalidCorrelation(format!(
                "call {correlation_id} has {} model requests",
                call.requests
            )));
        }
        if call.call_outcomes != 1 {
            return Err(TraceLoadError::InvalidModelLifecycle(format!(
                "call {correlation_id} does not have exactly one model establishment outcome"
            )));
        }
        if usize::try_from(call.next_started_attempt).unwrap_or(usize::MAX)
            != call.attempt_outcomes.len()
            || !(1..=call.next_started_attempt)
                .all(|attempt| call.attempt_outcomes.contains(&attempt))
        {
            return Err(TraceLoadError::InvalidCorrelation(format!(
                "call {correlation_id} has an attempt without exactly one outcome"
            )));
        }
        if call.call_established != (call.stream_established || call.model_events > 0) {
            return Err(TraceLoadError::InvalidModelLifecycle(format!(
                "call {correlation_id} has inconsistent establishment facts"
            )));
        }
        if call.call_failed && call.model_events > 0 {
            return Err(TraceLoadError::InvalidModelLifecycle(format!(
                "call {correlation_id} has model events after establishment failure"
            )));
        }
        if call.call_established && call.terminal_events != 1 {
            return Err(TraceLoadError::InvalidModelLifecycle(format!(
                "call {correlation_id} does not have exactly one model terminal"
            )));
        }
    }
    Ok(())
}

fn attempt_event_trace(event: &ModelAttemptEvent) -> Option<&TraceContext> {
    match event {
        ModelAttemptEvent::Started { trace, .. }
        | ModelAttemptEvent::EstablishmentFailed { trace, .. }
        | ModelAttemptEvent::RetryScheduled { trace, .. }
        | ModelAttemptEvent::StreamEstablished { trace, .. } => trace.as_ref(),
    }
}

fn wire_event_trace(event: &ProviderWireEvent) -> Option<&TraceContext> {
    match event {
        ProviderWireEvent::Request { trace, .. }
        | ProviderWireEvent::ResponseStarted { trace, .. }
        | ProviderWireEvent::ResponseChunk { trace, .. }
        | ProviderWireEvent::ResponseFailed { trace, .. }
        | ProviderWireEvent::ResponseFinished { trace } => trace.as_ref(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use agent_model::{
        GenerationConfig, ModelError, ModelRequest, ProviderOptions, SystemPromptSnapshot,
    };
    use agent_types::{ConversationSnapshot, ToolChoice};
    use tempfile::TempDir;

    use super::*;

    fn provider() -> ProviderMetadata {
        ProviderMetadata {
            adapter: "openai-compatible".into(),
            adapter_version: 1,
            protocol_adapter: "generic".into(),
            provider_id: "fixture".into(),
            protocol: "openai.chat_completions".into(),
            endpoint: "https://example.invalid/v1".into(),
            model: "fixture".into(),
            context_window_tokens: 4096,
        }
    }

    #[test]
    fn provider_metadata_reads_legacy_profile_field_as_protocol_adapter() {
        let legacy = r#"{
            "adapter":"openai-compatible",
            "adapter_version":1,
            "profile":"generic",
            "provider_id":"fixture",
            "protocol":"openai.chat_completions",
            "endpoint":"https://example.invalid/v1",
            "model":"fixture",
            "context_window_tokens":4096
        }"#;
        let metadata: ProviderMetadata = serde_json::from_str(legacy).expect("legacy metadata");
        assert_eq!(metadata.protocol_adapter, "generic");
    }

    fn started(id: &str) -> TraceLine {
        TraceLine::Started(TraceStarted {
            format_version: TRACE_FORMAT_VERSION,
            trace_id: id.into(),
            provider: provider(),
            started_at_ms: 1,
        })
    }

    fn host_record(sequence: u64) -> TraceLine {
        TraceLine::Record(TraceRecord {
            sequence,
            observed_at_ms: sequence + 1,
            layer: TraceLayer::Host,
            correlation_id: None,
            attempt: None,
            payload: NativeTracePayload::Host(DemoHostEvent::AgentExecutionStarted),
        })
    }

    fn completed(count: u64) -> TraceLine {
        TraceLine::Completed(TraceCompleted {
            last_sequence: count,
            record_count: count,
            completed_at_ms: 10,
        })
    }

    async fn write_lines(path: &Path, lines: &[TraceLine]) {
        let mut bytes = Vec::new();
        for line in lines {
            serde_json::to_writer(&mut bytes, line).unwrap();
            bytes.push(b'\n');
        }
        tokio::fs::write(path, bytes).await.unwrap();
    }

    #[tokio::test]
    async fn timeline_accepts_incomplete_but_exact_loader_rejects_it() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("trace-1.incomplete.jsonl");
        write_lines(&path, &[started("trace-1"), host_record(1)]).await;

        let loaded = load_for_timeline(&path).await.unwrap();
        assert_eq!(loaded.completeness, TraceCompleteness::Incomplete);
        assert!(matches!(
            load_complete(&path).await,
            Err(TraceLoadError::Incomplete)
        ));
    }

    #[tokio::test]
    async fn loader_rejects_missing_tail_torn_line_version_sequence_and_count() {
        let directory = TempDir::new().unwrap();

        let missing = directory.path().join("missing.jsonl");
        write_lines(&missing, &[started("missing"), host_record(1)]).await;
        assert!(matches!(
            load_for_timeline(&missing).await,
            Err(TraceLoadError::MissingCompletion)
        ));

        let torn = directory.path().join("torn.incomplete.jsonl");
        tokio::fs::write(&torn, b"{\"kind\":\"started\"\n")
            .await
            .unwrap();
        assert!(matches!(
            load_for_timeline(&torn).await,
            Err(TraceLoadError::Json { .. })
        ));

        let version = directory.path().join("version.jsonl");
        let mut wrong_version = started("version");
        let TraceLine::Started(start) = &mut wrong_version else {
            unreachable!()
        };
        start.format_version += 1;
        write_lines(&version, &[wrong_version, completed(0)]).await;
        assert!(matches!(
            load_complete(&version).await,
            Err(TraceLoadError::UnsupportedVersion(_))
        ));

        let sequence = directory.path().join("sequence.jsonl");
        write_lines(
            &sequence,
            &[started("sequence"), host_record(2), completed(1)],
        )
        .await;
        assert!(matches!(
            load_complete(&sequence).await,
            Err(TraceLoadError::SequenceMismatch { .. })
        ));

        let count = directory.path().join("count.jsonl");
        write_lines(&count, &[started("count"), host_record(1), completed(2)]).await;
        assert!(matches!(
            load_complete(&count).await,
            Err(TraceLoadError::CountMismatch)
        ));

        let base64 = directory.path().join("base64.incomplete.jsonl");
        let wire = TraceLine::Record(TraceRecord {
            sequence: 1,
            observed_at_ms: 2,
            layer: TraceLayer::Provider,
            correlation_id: Some("call-1".into()),
            attempt: None,
            payload: NativeTracePayload::ProviderWire(ProviderWireEvent::ResponseChunk {
                trace: Some(TraceContext::new("call-1")),
                bytes: vec![1, 2],
            }),
        });
        let mut invalid_base64 = serde_json::to_string(&started("base64")).unwrap();
        invalid_base64.push('\n');
        invalid_base64.push_str(&serde_json::to_string(&wire).unwrap().replace("AQI=", "%%%"));
        invalid_base64.push('\n');
        tokio::fs::write(&base64, invalid_base64).await.unwrap();
        assert!(matches!(
            load_for_timeline(&base64).await,
            Err(TraceLoadError::Json { .. })
        ));
    }

    #[tokio::test]
    async fn complete_loader_validates_correlation_attempts_and_model_terminal() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("model.jsonl");
        let logical_trace = TraceContext::new("call-1");
        let attempt_trace = logical_trace
            .clone()
            .with_attempt(NonZeroU32::new(1).unwrap());
        let request = ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        };
        let lines = vec![
            started("model"),
            TraceLine::Record(TraceRecord {
                sequence: 1,
                observed_at_ms: 2,
                layer: TraceLayer::Model,
                correlation_id: Some(logical_trace.correlation_id.clone()),
                attempt: None,
                payload: NativeTracePayload::ModelRequest(request),
            }),
            TraceLine::Record(TraceRecord {
                sequence: 2,
                observed_at_ms: 3,
                layer: TraceLayer::Model,
                correlation_id: Some(attempt_trace.correlation_id.clone()),
                attempt: Some(1),
                payload: NativeTracePayload::ModelAttempt(ModelAttemptEvent::Started {
                    trace: Some(attempt_trace.clone()),
                    attempt: 1,
                }),
            }),
            TraceLine::Record(TraceRecord {
                sequence: 3,
                observed_at_ms: 4,
                layer: TraceLayer::Model,
                correlation_id: Some(attempt_trace.correlation_id.clone()),
                attempt: Some(1),
                payload: NativeTracePayload::ModelAttempt(ModelAttemptEvent::StreamEstablished {
                    trace: Some(attempt_trace),
                    attempt: 1,
                }),
            }),
            TraceLine::Record(TraceRecord {
                sequence: 4,
                observed_at_ms: 5,
                layer: TraceLayer::Model,
                correlation_id: Some(logical_trace.correlation_id.clone()),
                attempt: None,
                payload: NativeTracePayload::ModelCall(ModelCallEvent::StreamEstablished),
            }),
            TraceLine::Record(TraceRecord {
                sequence: 5,
                observed_at_ms: 6,
                layer: TraceLayer::Model,
                correlation_id: Some(logical_trace.correlation_id),
                attempt: None,
                payload: NativeTracePayload::ModelEvent(ModelEvent::TurnFailed {
                    error: ModelError::Cancelled,
                }),
            }),
            completed(5),
        ];
        write_lines(&path, &lines).await;
        let loaded = load_complete(&path).await.unwrap();
        assert_eq!(loaded.records.len(), 5);

        let invalid = directory.path().join("invalid-attempt.jsonl");
        let mut invalid_lines = lines;
        let TraceLine::Started(start) = &mut invalid_lines[0] else {
            unreachable!()
        };
        start.trace_id = "invalid-attempt".into();
        let TraceLine::Record(record) = &mut invalid_lines[1] else {
            unreachable!()
        };
        record.attempt = Some(2);
        write_lines(&invalid, &invalid_lines).await;
        assert!(matches!(
            load_complete(&invalid).await,
            Err(TraceLoadError::InvalidCorrelation(_))
        ));
    }
}
