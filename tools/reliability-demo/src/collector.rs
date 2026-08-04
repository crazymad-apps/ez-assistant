//! 有界、非阻塞且不反向影响执行结果的 Demo Trace Collector。

use std::{
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use agent_model::{ModelAttemptEvent, ModelAttemptObserver, TraceContext};
use agent_provider_openai_compatible::{ProviderWireEvent, ProviderWireObserver};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    fs::{File, OpenOptions},
    io::AsyncWriteExt,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::trace::{
    NativeTracePayload, ProviderMetadata, TRACE_FORMAT_VERSION, TraceCompleted, TraceLine,
    TraceRecord, TraceStarted,
};

const STATE_ACCEPTING: u8 = 0;
const STATE_FINISHING: u8 = 1;
const STATE_INCOMPLETE: u8 = 2;
const STATE_COMPLETE: u8 = 3;

pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 4096;
pub(crate) const DEFAULT_MAX_TRACE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CollectorConfig {
    pub(crate) queue_capacity: NonZeroUsize,
    pub(crate) max_trace_bytes: NonZeroU64,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            queue_capacity: NonZeroUsize::new(DEFAULT_QUEUE_CAPACITY)
                .expect("default queue capacity is non-zero"),
            max_trace_bytes: NonZeroU64::new(DEFAULT_MAX_TRACE_BYTES)
                .expect("default max trace bytes is non-zero"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IncompleteReason {
    QueueFull,
    SerializationFailed,
    WriteFailed,
    FlushFailed,
    RenameFailed,
    MaxBytesExceeded,
    AgentEventsDropped,
    HostEndedEarly,
    WriterUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecordAcceptance {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollectorCompleteness {
    Complete,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TraceSummary {
    pub(crate) path: PathBuf,
    pub(crate) completeness: CollectorCompleteness,
    pub(crate) incomplete_reason: Option<IncompleteReason>,
    pub(crate) record_count: u64,
    pub(crate) file_bytes: u64,
}

#[derive(Debug, Error)]
pub(crate) enum CollectorStartError {
    #[error("failed to create trace data directory: {0}")]
    CreateDirectory(#[source] std::io::Error),
    #[error("failed to create trace file: {0}")]
    CreateFile(#[source] std::io::Error),
    #[error("failed to serialize trace header: {0}")]
    SerializeHeader(#[source] serde_json::Error),
    #[error("trace byte limit is too small for the version header")]
    HeaderExceedsLimit,
    #[error("failed to write trace header: {0}")]
    WriteHeader(#[source] std::io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum CollectorFinishError {
    #[error("trace writer task stopped unexpectedly: {0}")]
    WriterTask(#[source] tokio::task::JoinError),
}

struct SharedState {
    state: AtomicU8,
    incomplete_reason: Mutex<Option<IncompleteReason>>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(STATE_ACCEPTING),
            incomplete_reason: Mutex::new(None),
        }
    }

    fn is_accepting(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_ACCEPTING
    }

    fn begin_finishing(&self) {
        let _ = self.state.compare_exchange(
            STATE_ACCEPTING,
            STATE_FINISHING,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn mark_incomplete(&self, reason: IncompleteReason) {
        // 状态发布与首个原因写入共用这把短锁：其他线程一旦观察到 Incomplete，
        // 读取原因时会等待首个写入者完成，不会得到短暂的 `None`。
        let mut incomplete_reason = self
            .incomplete_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current == STATE_INCOMPLETE || current == STATE_COMPLETE {
                return;
            }
            if self
                .state
                .compare_exchange(
                    current,
                    STATE_INCOMPLETE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                *incomplete_reason = Some(reason);
                return;
            }
        }
    }

    fn mark_complete(&self) -> bool {
        self.state
            .compare_exchange(
                STATE_FINISHING,
                STATE_COMPLETE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn is_incomplete(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_INCOMPLETE
    }

    fn incomplete_reason(&self) -> Option<IncompleteReason> {
        *self
            .incomplete_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct PendingRecord {
    observed_at_ms: u64,
    correlation_id: Option<String>,
    attempt: Option<u32>,
    payload: NativeTracePayload,
}

/// 可克隆到各原生观察器中的非阻塞发送端。
#[derive(Clone)]
pub(crate) struct TraceSink {
    sender: mpsc::Sender<PendingRecord>,
    shared: Arc<SharedState>,
}

impl TraceSink {
    /// 使用 payload 自带的 TraceContext；适合 attempt 与 Provider wire 事实。
    pub(crate) fn record(&self, payload: NativeTracePayload) -> RecordAcceptance {
        let trace = payload.native_trace().cloned();
        self.record_with_trace(trace.as_ref(), payload)
    }

    /// 为本身不携带 TraceContext 的 Model/Agent/Host 事实建立宿主关联。
    pub(crate) fn record_with_trace(
        &self,
        trace: Option<&TraceContext>,
        payload: NativeTracePayload,
    ) -> RecordAcceptance {
        if !self.shared.is_accepting() {
            return RecordAcceptance::Rejected;
        }
        let pending = PendingRecord {
            observed_at_ms: unix_time_ms(),
            correlation_id: trace.map(|trace| trace.correlation_id.clone()),
            attempt: trace.and_then(|trace| trace.attempt.map(|attempt| attempt.get())),
            payload,
        };
        match self.sender.try_send(pending) {
            Ok(()) => RecordAcceptance::Accepted,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.shared.mark_incomplete(IncompleteReason::QueueFull);
                RecordAcceptance::Rejected
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.shared
                    .mark_incomplete(IncompleteReason::WriterUnavailable);
                RecordAcceptance::Rejected
            }
        }
    }

    /// 上游观察通道已经明确丢失事实时，将整个 Trace 降级为 Incomplete。
    pub(crate) fn mark_incomplete(&self, reason: IncompleteReason) {
        self.shared.mark_incomplete(reason);
    }
}

impl ModelAttemptObserver for TraceSink {
    fn observe(&self, event: ModelAttemptEvent) {
        let _ = self.record(NativeTracePayload::ModelAttempt(event));
    }
}

impl ProviderWireObserver for TraceSink {
    fn observe(&self, event: ProviderWireEvent) {
        let _ = self.record(NativeTracePayload::ProviderWire(event));
    }
}

/// 拥有单个 Trace writer 和最终 commit point；记录入口由 [`TraceSink`] 提供。
pub(crate) struct TraceCollector {
    sink: TraceSink,
    shutdown: watch::Sender<bool>,
    writer: JoinHandle<TraceSummary>,
    shared: Arc<SharedState>,
}

impl TraceCollector {
    pub(crate) async fn start(
        data_dir: &Path,
        provider: ProviderMetadata,
        config: CollectorConfig,
    ) -> Result<Self, CollectorStartError> {
        Self::start_inner(data_dir, provider, config, WriterFaults::default()).await
    }

    #[cfg(test)]
    async fn start_with_faults(
        data_dir: &Path,
        provider: ProviderMetadata,
        config: CollectorConfig,
        faults: WriterFaults,
    ) -> Result<Self, CollectorStartError> {
        Self::start_inner(data_dir, provider, config, faults).await
    }

    async fn start_inner(
        data_dir: &Path,
        provider: ProviderMetadata,
        config: CollectorConfig,
        faults: WriterFaults,
    ) -> Result<Self, CollectorStartError> {
        tokio::fs::create_dir_all(data_dir)
            .await
            .map_err(CollectorStartError::CreateDirectory)?;
        let trace_id = next_trace_id();
        let incomplete_path = data_dir.join(format!("{trace_id}.incomplete.jsonl"));
        let complete_path = data_dir.join(format!("{trace_id}.jsonl"));
        let started = TraceLine::Started(TraceStarted {
            format_version: TRACE_FORMAT_VERSION,
            trace_id,
            provider,
            started_at_ms: unix_time_ms(),
        });
        let header = json_line(&started).map_err(CollectorStartError::SerializeHeader)?;
        if u64::try_from(header.len()).unwrap_or(u64::MAX) > config.max_trace_bytes.get() {
            return Err(CollectorStartError::HeaderExceedsLimit);
        }
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&incomplete_path)
            .await
            .map_err(CollectorStartError::CreateFile)?;
        let mut file = file;
        file.write_all(&header)
            .await
            .map_err(CollectorStartError::WriteHeader)?;

        let shared = Arc::new(SharedState::new());
        let (sender, receiver) = mpsc::channel(config.queue_capacity.get());
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let sink = TraceSink {
            sender,
            shared: shared.clone(),
        };
        let writer_shared = shared.clone();
        let writer = tokio::spawn(run_writer(WriterContext {
            file,
            receiver,
            shutdown: shutdown_receiver,
            incomplete_path,
            complete_path,
            max_trace_bytes: config.max_trace_bytes.get(),
            file_bytes: u64::try_from(header.len()).unwrap_or(u64::MAX),
            shared: writer_shared,
            faults,
        }));
        Ok(Self {
            sink,
            shutdown,
            writer,
            shared,
        })
    }

    pub(crate) fn sink(&self) -> TraceSink {
        self.sink.clone()
    }

    /// 正常结束：关闭接收端、排空此前已接受记录，并尝试写完成尾和原子 rename。
    pub(crate) async fn finish(self) -> Result<TraceSummary, CollectorFinishError> {
        self.shared.begin_finishing();
        let _ = self.shutdown.send(true);
        self.writer.await.map_err(CollectorFinishError::WriterTask)
    }

    /// 宿主提前结束只保留 `.incomplete.jsonl`，但仍排空已接受记录。
    pub(crate) async fn abort(self) -> Result<TraceSummary, CollectorFinishError> {
        self.shared
            .mark_incomplete(IncompleteReason::HostEndedEarly);
        let _ = self.shutdown.send(true);
        self.writer.await.map_err(CollectorFinishError::WriterTask)
    }
}

#[derive(Clone, Copy, Default)]
struct WriterFaults {
    #[cfg(test)]
    serialize_at_sequence: Option<u64>,
    #[cfg(test)]
    write_at_sequence: Option<u64>,
    #[cfg(test)]
    fail_flush: bool,
    #[cfg(test)]
    fail_rename: bool,
}

struct WriterContext {
    file: File,
    receiver: mpsc::Receiver<PendingRecord>,
    shutdown: watch::Receiver<bool>,
    incomplete_path: PathBuf,
    complete_path: PathBuf,
    max_trace_bytes: u64,
    file_bytes: u64,
    shared: Arc<SharedState>,
    // 生产 binary 中不读取故障注入配置；自动测试会读取每个字段。
    #[cfg_attr(not(test), allow(dead_code))]
    faults: WriterFaults,
}

async fn run_writer(mut context: WriterContext) -> TraceSummary {
    let mut record_count = 0_u64;
    let mut can_write = true;
    let mut shutting_down = false;

    while !shutting_down {
        tokio::select! {
            changed = context.shutdown.changed() => {
                if changed.is_err() {
                    context.shared.mark_incomplete(IncompleteReason::HostEndedEarly);
                    context.receiver.close();
                    shutting_down = true;
                } else if *context.shutdown.borrow() {
                    context.receiver.close();
                    shutting_down = true;
                }
            }
            pending = context.receiver.recv() => {
                let Some(pending) = pending else {
                    if context.shared.is_accepting() {
                        context.shared.mark_incomplete(IncompleteReason::HostEndedEarly);
                    }
                    shutting_down = true;
                    continue;
                };
                write_pending(&mut context, pending, &mut record_count, &mut can_write).await;
            }
        }
    }
    while let Some(pending) = context.receiver.recv().await {
        write_pending(&mut context, pending, &mut record_count, &mut can_write).await;
    }

    // 先确保头和全部记录已经完成写入，再追加完成尾。这样完成尾的失败可以
    // 按已知偏移截回，`.incomplete.jsonl` 不会留下可误判的完整尾。
    if !context.shared.is_incomplete() && can_write && context.file.flush().await.is_err() {
        context
            .shared
            .mark_incomplete(IncompleteReason::FlushFailed);
        can_write = false;
    }

    let bytes_before_tail = context.file_bytes;
    if !context.shared.is_incomplete() && can_write {
        let tail = TraceLine::Completed(TraceCompleted {
            last_sequence: record_count,
            record_count,
            completed_at_ms: unix_time_ms(),
        });
        match json_line(&tail) {
            Ok(line) if fits_limit(context.file_bytes, line.len(), context.max_trace_bytes) => {
                if context.file.write_all(&line).await.is_ok() {
                    context.file_bytes += u64::try_from(line.len()).unwrap_or(u64::MAX);
                } else {
                    let _ = context.file.set_len(bytes_before_tail).await;
                    context.file_bytes = bytes_before_tail;
                    context
                        .shared
                        .mark_incomplete(IncompleteReason::WriteFailed);
                    can_write = false;
                }
            }
            Ok(_) => {
                context
                    .shared
                    .mark_incomplete(IncompleteReason::MaxBytesExceeded);
                can_write = false;
            }
            Err(_) => {
                context
                    .shared
                    .mark_incomplete(IncompleteReason::SerializationFailed);
                can_write = false;
            }
        }
    }

    if can_write && !context.shared.is_incomplete() {
        let flush_failed = {
            #[cfg(test)]
            {
                context.faults.fail_flush
            }
            #[cfg(not(test))]
            {
                false
            }
        };
        if flush_failed || context.file.flush().await.is_err() {
            let _ = context.file.set_len(bytes_before_tail).await;
            context.file_bytes = bytes_before_tail;
            context
                .shared
                .mark_incomplete(IncompleteReason::FlushFailed);
        }
    } else if can_write {
        let _ = context.file.flush().await;
    }

    if !context.shared.is_incomplete() {
        drop(context.file);
        let rename_failed = {
            #[cfg(test)]
            {
                context.faults.fail_rename
                    || tokio::fs::rename(&context.incomplete_path, &context.complete_path)
                        .await
                        .is_err()
            }
            #[cfg(not(test))]
            {
                tokio::fs::rename(&context.incomplete_path, &context.complete_path)
                    .await
                    .is_err()
            }
        };
        if rename_failed {
            if let Ok(file) = OpenOptions::new()
                .write(true)
                .open(&context.incomplete_path)
                .await
            {
                let _ = file.set_len(bytes_before_tail).await;
            }
            context.file_bytes = bytes_before_tail;
            context
                .shared
                .mark_incomplete(IncompleteReason::RenameFailed);
        } else {
            let _ = context.shared.mark_complete();
        }
    }

    let is_complete = context.shared.state.load(Ordering::Acquire) == STATE_COMPLETE;
    TraceSummary {
        path: if is_complete {
            context.complete_path
        } else {
            context.incomplete_path
        },
        completeness: if is_complete {
            CollectorCompleteness::Complete
        } else {
            CollectorCompleteness::Incomplete
        },
        incomplete_reason: context.shared.incomplete_reason(),
        record_count,
        file_bytes: context.file_bytes,
    }
}

async fn write_pending(
    context: &mut WriterContext,
    pending: PendingRecord,
    record_count: &mut u64,
    can_write: &mut bool,
) {
    if !*can_write {
        return;
    }
    let sequence = record_count.saturating_add(1);
    #[cfg(test)]
    if context.faults.serialize_at_sequence == Some(sequence) {
        context
            .shared
            .mark_incomplete(IncompleteReason::SerializationFailed);
        *can_write = false;
        return;
    }
    let line = match json_line(&TraceLine::Record(TraceRecord {
        sequence,
        observed_at_ms: pending.observed_at_ms,
        layer: pending.payload.layer(),
        correlation_id: pending.correlation_id,
        attempt: pending.attempt,
        payload: pending.payload,
    })) {
        Ok(line) => line,
        Err(_) => {
            context
                .shared
                .mark_incomplete(IncompleteReason::SerializationFailed);
            *can_write = false;
            return;
        }
    };
    if !fits_limit(context.file_bytes, line.len(), context.max_trace_bytes) {
        context
            .shared
            .mark_incomplete(IncompleteReason::MaxBytesExceeded);
        *can_write = false;
        return;
    }
    #[cfg(test)]
    if context.faults.write_at_sequence == Some(sequence) {
        context
            .shared
            .mark_incomplete(IncompleteReason::WriteFailed);
        *can_write = false;
        return;
    }
    if context.file.write_all(&line).await.is_err() {
        context
            .shared
            .mark_incomplete(IncompleteReason::WriteFailed);
        *can_write = false;
        return;
    }
    context.file_bytes += u64::try_from(line.len()).unwrap_or(u64::MAX);
    *record_count = sequence;
}

fn fits_limit(current: u64, additional: usize, limit: u64) -> bool {
    current
        .checked_add(u64::try_from(additional).unwrap_or(u64::MAX))
        .is_some_and(|total| total <= limit)
}

fn json_line(value: &TraceLine) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    Ok(line)
}

fn unix_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn next_trace_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{nanos:x}-{:x}-{counter:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64},
        sync::Arc,
    };

    use agent_core::AgentEvent;
    use agent_model::{
        GenerationConfig, ModelAttemptEvent, ModelError, ModelRequest, ModelTransportErrorKind,
        ProviderOptions, SystemPromptSnapshot, TraceContext,
    };
    use agent_provider_openai_compatible::{
        ProviderWireEvent, RecordedWireRequest, TransportError,
    };
    use agent_types::{ConversationSnapshot, ToolChoice};
    use tempfile::TempDir;

    use super::*;
    use crate::trace::{ModelCallEvent, NativeTracePayload, TraceCompleteness, load_for_timeline};

    fn provider() -> ProviderMetadata {
        ProviderMetadata {
            adapter: "openai-compatible".into(),
            adapter_version: 1,
            profile: "generic".into(),
            provider_id: "fixture".into(),
            protocol: "openai.chat_completions".into(),
            endpoint: "https://example.invalid/v1".into(),
            model: "fixture-model".into(),
            context_window_tokens: 4096,
        }
    }

    fn model_request() -> ModelRequest {
        ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }

    #[tokio::test]
    async fn collector_writes_complete_file_and_native_observer_facts() {
        let directory = TempDir::new().unwrap();
        let collector =
            TraceCollector::start(directory.path(), provider(), CollectorConfig::default())
                .await
                .unwrap();
        let sink = collector.sink();
        let logical_trace = TraceContext::new("call-1");
        let _ = sink.record_with_trace(
            Some(&logical_trace),
            NativeTracePayload::ModelRequest(model_request()),
        );
        let trace = logical_trace.with_attempt(NonZeroU32::new(1).unwrap());
        ModelAttemptObserver::observe(
            &sink,
            ModelAttemptEvent::Started {
                trace: Some(trace.clone()),
                attempt: 1,
            },
        );
        ProviderWireObserver::observe(
            &sink,
            ProviderWireEvent::Request {
                trace: Some(trace.clone()),
                request: RecordedWireRequest {
                    method: "POST".into(),
                    url: "https://example.invalid/v1/chat/completions".into(),
                    headers: vec![],
                    body: b"{}".to_vec(),
                },
            },
        );
        ProviderWireObserver::observe(
            &sink,
            ProviderWireEvent::ResponseFailed {
                trace: Some(trace.clone()),
                error: TransportError::Connect("fixture refused".into()),
            },
        );
        let establishment_error = ModelError::Transport {
            kind: ModelTransportErrorKind::Connection,
            message: "fixture refused".into(),
        };
        ModelAttemptObserver::observe(
            &sink,
            ModelAttemptEvent::EstablishmentFailed {
                trace: Some(trace),
                attempt: 1,
                error: establishment_error.clone(),
                retry_reason: None,
                will_retry: false,
            },
        );
        let _ = sink.record_with_trace(
            Some(&TraceContext::new("call-1")),
            NativeTracePayload::ModelCall(ModelCallEvent::EstablishmentFailed {
                error: establishment_error,
            }),
        );
        drop(sink);
        let summary = collector.finish().await.unwrap();
        assert_eq!(summary.completeness, CollectorCompleteness::Complete);
        assert!(summary.path.to_string_lossy().ends_with(".jsonl"));
        let loaded = load_for_timeline(&summary.path).await.unwrap();
        assert_eq!(loaded.completeness, TraceCompleteness::Complete);
        assert_eq!(loaded.records.len(), 6);
        assert_eq!(loaded.records[0].sequence, 1);
        assert_eq!(loaded.records[1].sequence, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_queue_marks_incomplete_without_blocking_sender() {
        let directory = TempDir::new().unwrap();
        let config = CollectorConfig {
            queue_capacity: NonZeroUsize::new(1).unwrap(),
            ..CollectorConfig::default()
        };
        let collector = TraceCollector::start(directory.path(), provider(), config)
            .await
            .unwrap();
        let sink = collector.sink();
        assert_eq!(
            sink.record(NativeTracePayload::Agent(AgentEvent::ExecutionStarted)),
            RecordAcceptance::Accepted
        );
        assert_eq!(
            sink.record(NativeTracePayload::Agent(AgentEvent::StepStarted {
                step: 1
            })),
            RecordAcceptance::Rejected
        );
        let summary = collector.finish().await.unwrap();
        assert_eq!(summary.completeness, CollectorCompleteness::Incomplete);
        assert_eq!(summary.incomplete_reason, Some(IncompleteReason::QueueFull));
        assert!(
            summary
                .path
                .to_string_lossy()
                .ends_with(".incomplete.jsonl")
        );
    }

    #[tokio::test]
    async fn byte_limit_and_injected_failures_preserve_incomplete_file() {
        for (faults, expected) in [
            (
                WriterFaults {
                    serialize_at_sequence: Some(1),
                    ..WriterFaults::default()
                },
                IncompleteReason::SerializationFailed,
            ),
            (
                WriterFaults {
                    write_at_sequence: Some(1),
                    ..WriterFaults::default()
                },
                IncompleteReason::WriteFailed,
            ),
            (
                WriterFaults {
                    fail_flush: true,
                    ..WriterFaults::default()
                },
                IncompleteReason::FlushFailed,
            ),
            (
                WriterFaults {
                    fail_rename: true,
                    ..WriterFaults::default()
                },
                IncompleteReason::RenameFailed,
            ),
        ] {
            let directory = TempDir::new().unwrap();
            let collector = TraceCollector::start_with_faults(
                directory.path(),
                provider(),
                CollectorConfig::default(),
                faults,
            )
            .await
            .unwrap();
            let _ = collector
                .sink()
                .record(NativeTracePayload::Agent(AgentEvent::ExecutionStarted));
            let summary = collector.finish().await.unwrap();
            assert_eq!(summary.completeness, CollectorCompleteness::Incomplete);
            assert_eq!(summary.incomplete_reason, Some(expected));
            let loaded = load_for_timeline(&summary.path).await.unwrap();
            assert_eq!(loaded.completeness, TraceCompleteness::Incomplete);
            assert!(loaded.completed.is_none());
        }

        let directory = TempDir::new().unwrap();
        let collector = TraceCollector::start(
            directory.path(),
            provider(),
            CollectorConfig {
                queue_capacity: NonZeroUsize::new(8).unwrap(),
                max_trace_bytes: NonZeroU64::new(700).unwrap(),
            },
        )
        .await
        .unwrap();
        let _ = collector.sink().record(NativeTracePayload::ProviderWire(
            ProviderWireEvent::ResponseChunk {
                trace: Some(TraceContext::new("large").with_attempt(NonZeroU32::new(1).unwrap())),
                bytes: vec![7; 1024],
            },
        ));
        let summary = collector.finish().await.unwrap();
        assert_eq!(
            summary.incomplete_reason,
            Some(IncompleteReason::MaxBytesExceeded)
        );
    }

    #[tokio::test]
    async fn abort_and_post_failure_records_are_rejected() {
        let directory = TempDir::new().unwrap();
        let collector =
            TraceCollector::start(directory.path(), provider(), CollectorConfig::default())
                .await
                .unwrap();
        let sink = collector.sink();
        let summary = collector.abort().await.unwrap();
        assert_eq!(
            summary.incomplete_reason,
            Some(IncompleteReason::HostEndedEarly)
        );
        assert_eq!(
            sink.record(NativeTracePayload::Agent(AgentEvent::ExecutionStarted)),
            RecordAcceptance::Rejected
        );
    }

    #[tokio::test]
    async fn dropping_owner_without_finish_cannot_commit_complete_trace() {
        let directory = TempDir::new().unwrap();
        let collector =
            TraceCollector::start(directory.path(), provider(), CollectorConfig::default())
                .await
                .unwrap();
        let _ = collector
            .sink()
            .record(NativeTracePayload::Agent(AgentEvent::ExecutionStarted));
        let TraceCollector {
            sink,
            shutdown,
            writer,
            shared,
        } = collector;
        drop(sink);
        drop(shutdown);
        drop(shared);
        let summary = writer.await.unwrap();
        assert_eq!(summary.completeness, CollectorCompleteness::Incomplete);
        assert_eq!(
            summary.incomplete_reason,
            Some(IncompleteReason::HostEndedEarly)
        );
        assert!(
            summary
                .path
                .to_string_lossy()
                .ends_with(".incomplete.jsonl")
        );
    }

    #[tokio::test]
    async fn concurrent_producers_get_global_sequence_and_keep_local_order() {
        let directory = TempDir::new().unwrap();
        let collector =
            TraceCollector::start(directory.path(), provider(), CollectorConfig::default())
                .await
                .unwrap();
        let sink = Arc::new(collector.sink());
        let mut tasks = Vec::new();
        for call in 0..4_u32 {
            let sink = sink.clone();
            tasks.push(tokio::spawn(async move {
                let trace = TraceContext::new(format!("agent-{call}"));
                for step in 1..=20_u32 {
                    assert_eq!(
                        sink.record_with_trace(
                            Some(&trace),
                            NativeTracePayload::Agent(AgentEvent::StepStarted { step }),
                        ),
                        RecordAcceptance::Accepted
                    );
                    tokio::task::yield_now().await;
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        drop(sink);
        let summary = collector.finish().await.unwrap();
        let loaded = load_for_timeline(&summary.path).await.unwrap();
        assert_eq!(loaded.records.len(), 80);
        for (index, record) in loaded.records.iter().enumerate() {
            assert_eq!(record.sequence, u64::try_from(index + 1).unwrap());
        }
        for call in 0..4_u32 {
            let steps = loaded
                .records
                .iter()
                .filter(|record| record.correlation_id.as_deref() == Some(&format!("agent-{call}")))
                .map(|record| match record.payload {
                    NativeTracePayload::Agent(AgentEvent::StepStarted { step }) => step,
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>();
            assert_eq!(steps, (1..=20).collect::<Vec<_>>());
        }
    }
}
