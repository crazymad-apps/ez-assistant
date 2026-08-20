//! 真实 Provider、Agent Core 与 Trace Collector 的 Demo 私有装配。

use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use agent_context::ContextWindowEvaluator;
use agent_core::{
    AgentExecution, AllowAllAuthorizer, ExchangeReceipt, ExecutionBudget, ExecutionContext,
    ExecutionInput, ExecutionOutcome, ExecutionRecorder, ExecutionSpec, ModelRequestConfig,
    RecordError, RecordFuture,
};
use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ModelEventStream, ModelRequest,
    ModelRetryPolicy, ModelService, ModelStreamFuture, ProviderOptions, ReasoningConfig,
    RetryingModelService, SystemPromptSnapshot, TraceContext,
};
use agent_openai_compatible::{
    BearerCredential, ChatProtocolAdapter, ObservedTransport, OpenAiChatCompletionsService,
    ReqwestTransport, Transport, TransportTimeouts,
};
use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolRegistry, ToolResolution};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, ToolChoice, ToolName,
    UserMessage, UserPart,
};
use futures_util::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    collector::{
        CollectorCompleteness, CollectorConfig, CollectorFinishError, CollectorStartError,
        IncompleteReason, TraceCollector, TraceSink, TraceSummary,
    },
    replay::{
        OPENAI_CHAT_COMPLETIONS_PROTOCOL, OPENAI_COMPATIBLE_ADAPTER,
        OPENAI_COMPATIBLE_ADAPTER_VERSION,
    },
    trace::{
        DemoHostEvent, LoadedTrace, ModelCallEvent, NativeTracePayload, ProviderMetadata,
        TraceLayer,
    },
};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const SYSTEM_PROMPT: &str = "You are the reliability demo agent. Before answering the user, call \
reliability_probe exactly once with a short non-sensitive label describing the request. Then answer \
concisely. The probe is deterministic and has no external side effects.";

fn deepseek_model_request_config() -> ModelRequestConfig {
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert(
            "deepseek",
            serde_json::json!({"thinking": {"type": "enabled"}}),
        )
        .expect("static DeepSeek provider options are valid");
    ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: Some(ReasoningConfig { effort: None }),
        provider_options,
    }
}

/// `record` 命令已经完成语法校验的显式参数。
pub(crate) struct RecordOptions {
    pub(crate) data_dir: PathBuf,
    pub(crate) collector: CollectorConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
}

#[derive(Debug, Error)]
pub(crate) enum RecordCommandError {
    #[error("failed to read the task from standard input: {0}")]
    ReadTask(#[source] std::io::Error),
    #[error("the task read from standard input is empty")]
    EmptyTask,
    #[error("missing DEEPSEEK_API_KEY; configure it in the process environment or repository .env")]
    MissingCredential,
    #[error("DEEPSEEK_CONTEXT_WINDOW_TOKENS must be a positive integer")]
    InvalidContextWindow,
    #[error("failed to construct the Provider adapter: {0}")]
    Provider(String),
    #[error("failed to construct the context window evaluator: {0}")]
    ContextWindow(String),
    #[error("failed to register the fixed reliability tool: {0}")]
    ToolRegistry(String),
    #[error(transparent)]
    CollectorStart(#[from] CollectorStartError),
    #[error(transparent)]
    CollectorFinish(#[from] CollectorFinishError),
    #[error("the Agent event drain stopped unexpectedly: {0}")]
    AgentEventDrain(#[source] tokio::task::JoinError),
    #[error("the Agent event stream ended without its reliable terminal event")]
    AgentEventMissingTerminal,
    #[error("the Agent execution did not complete successfully ({0})")]
    Execution(&'static str),
    #[error("failed to inspect the completed Trace: {0}")]
    TraceInspection(String),
}

struct ProviderConfig {
    api_key: String,
    endpoint: String,
    model: String,
    context_window_tokens: u64,
}

/// 只有这个入口加载 `.env`、credential，并构造真实网络 Transport。
pub(crate) async fn run(options: RecordOptions) -> Result<(), RecordCommandError> {
    dotenvy::dotenv().ok();
    let provider = provider_config()?;

    let raw_transport: Arc<dyn Transport> = Arc::new(
        ReqwestTransport::with_timeouts(TransportTimeouts::default())
            .map_err(|error| RecordCommandError::Provider(error.to_string()))?,
    );
    // 在 Trace header 落盘前先走 Adapter 的 URL 安全校验，避免把含认证信息的
    // endpoint 写入高敏文件。这个临时 Service 不会发出网络请求。
    let validated_service = OpenAiChatCompletionsService::with_transport(
        provider.endpoint.clone(),
        BearerCredential::new(provider.api_key.clone()),
        provider.model.clone(),
        provider.context_window_tokens,
        ChatProtocolAdapter::deepseek(),
        raw_transport.clone(),
    )
    .map_err(|error| RecordCommandError::Provider(error.to_string()))?;
    drop(validated_service);
    // credential、Provider 配置和 endpoint 先完成校验，缺配置时不阻塞等待 stdin。
    let task = read_task()?;

    let tools = tool_snapshot()?;
    let input = execution_input(task)?;
    let context_window = Arc::new(
        ContextWindowEvaluator::new(0.8)
            .map_err(|error| RecordCommandError::ContextWindow(error.to_string()))?,
    );
    let metadata = ProviderMetadata {
        adapter: OPENAI_COMPATIBLE_ADAPTER.into(),
        adapter_version: OPENAI_COMPATIBLE_ADAPTER_VERSION,
        protocol_adapter: "deepseek".into(),
        provider_id: "deepseek".into(),
        protocol: OPENAI_CHAT_COMPLETIONS_PROTOCOL.into(),
        endpoint: provider.endpoint.clone(),
        model: provider.model.clone(),
        context_window_tokens: provider.context_window_tokens,
    };
    let collector = TraceCollector::start(&options.data_dir, metadata, options.collector).await?;
    let sink = collector.sink();

    let observed_transport: Arc<dyn Transport> = Arc::new(ObservedTransport::new(
        raw_transport,
        Arc::new(sink.clone()),
    ));
    let provider_service = match OpenAiChatCompletionsService::with_transport(
        provider.endpoint.clone(),
        BearerCredential::new(provider.api_key.clone()),
        provider.model.clone(),
        provider.context_window_tokens,
        ChatProtocolAdapter::deepseek(),
        observed_transport,
    ) {
        Ok(service) => service,
        Err(error) => {
            let _ = collector.abort().await;
            return Err(RecordCommandError::Provider(error.to_string()));
        }
    };
    let provider_service: Arc<dyn ModelService> = Arc::new(provider_service);
    let model: Arc<dyn ModelService> = match options.retry_policy.clone() {
        Some(policy) => Arc::new(RetryingModelService::with_observer(
            provider_service,
            policy,
            Arc::new(sink.clone()),
        )),
        None => provider_service,
    };
    let model: Arc<dyn ModelService> = Arc::new(ObservedModelService::new(model, sink.clone()));

    print_configuration(&provider, &options, model.as_ref());
    let recorder = Arc::new(DemoJournal::new(sink.clone()));
    let context = ExecutionContext {
        cancellation: CancellationToken::new(),
        recorder,
        authorizer: Arc::new(AllowAllAuthorizer),
    };
    let spec = ExecutionSpec {
        system_prompt: SystemPromptSnapshot::new(vec![SYSTEM_PROMPT.into()]),
        model,
        context_window,
        tools,
        model_request: deepseek_model_request_config(),
        // Demo 用显式小边界限制真实费用和重复工具调用，不属于 Core 隐藏默认。
        budget: ExecutionBudget {
            max_steps: Some(4),
            max_tool_calls: Some(1),
        },
        guardrails: None,
    };

    let _ = sink.record(NativeTracePayload::Host(
        DemoHostEvent::AgentExecutionStarted,
    ));
    let AgentExecution {
        mut events,
        mut completion,
        control,
    } = AgentExecution::start(spec, input, context);
    let event_sink = sink.clone();
    let event_drain = tokio::spawn(async move {
        let mut terminal_seen = false;
        while let Some(event) = events.next().await {
            terminal_seen |= event.is_terminal();
            record_agent_event(&event_sink, event);
        }
        terminal_seen
    });

    let outcome = tokio::select! {
        outcome = &mut completion => outcome,
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                let _ = sink.record(NativeTracePayload::Host(
                    DemoHostEvent::CancellationRequested,
                ));
                control.cancel();
            }
            completion.await
        }
    };
    match event_drain.await {
        Ok(true) => {}
        Ok(false) => {
            let _ = collector.abort().await;
            return Err(RecordCommandError::AgentEventMissingTerminal);
        }
        Err(error) => {
            let _ = collector.abort().await;
            return Err(RecordCommandError::AgentEventDrain(error));
        }
    }
    let _ = sink.record(NativeTracePayload::Host(
        DemoHostEvent::AgentExecutionFinished {
            outcome: outcome.clone(),
        },
    ));
    let summary = collector.finish().await?;
    print_summary(&summary, &outcome).await?;

    match outcome {
        ExecutionOutcome::Completed(_) => Ok(()),
        ExecutionOutcome::Failed(_) => Err(RecordCommandError::Execution("failed")),
        ExecutionOutcome::Cancelled => Err(RecordCommandError::Execution("cancelled")),
        ExecutionOutcome::CompactionRequired { .. } => {
            Err(RecordCommandError::Execution("compaction required"))
        }
    }
}

fn read_task() -> Result<String, RecordCommandError> {
    let mut task = String::new();
    std::io::stdin()
        .read_to_string(&mut task)
        .map_err(RecordCommandError::ReadTask)?;
    let task = task.trim();
    if task.is_empty() {
        Err(RecordCommandError::EmptyTask)
    } else {
        Ok(task.to_owned())
    }
}

fn provider_config() -> Result<ProviderConfig, RecordCommandError> {
    provider_config_from(|name| std::env::var(name).ok())
}

fn provider_config_from(
    mut read: impl FnMut(&str) -> Option<String>,
) -> Result<ProviderConfig, RecordCommandError> {
    let api_key = read("DEEPSEEK_API_KEY")
        .filter(|value| !value.trim().is_empty())
        .ok_or(RecordCommandError::MissingCredential)?;
    let endpoint = read("DEEPSEEK_BASE_URL")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let model = read("DEEPSEEK_MODEL")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    let context_window_tokens = read("DEEPSEEK_CONTEXT_WINDOW_TOKENS")
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| RecordCommandError::InvalidContextWindow)?
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
    if context_window_tokens == 0 {
        return Err(RecordCommandError::InvalidContextWindow);
    }
    Ok(ProviderConfig {
        api_key,
        endpoint,
        model,
        context_window_tokens,
    })
}

fn tool_snapshot() -> Result<agent_tools::ToolSetSnapshot, RecordCommandError> {
    let mut registry = ToolRegistry::new();
    registry
        .register(ReliabilityProbe)
        .map_err(|error| RecordCommandError::ToolRegistry(error.to_string()))?;
    Ok(registry.snapshot())
}

fn execution_input(task: String) -> Result<ExecutionInput, RecordCommandError> {
    let message_id = MessageId::new("reliability-demo-user-1")
        .map_err(|error| RecordCommandError::Provider(error.to_string()))?;
    let part_id = PartId::new("reliability-demo-user-text-1")
        .map_err(|error| RecordCommandError::Provider(error.to_string()))?;
    Ok(ExecutionInput {
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: message_id,
            parts: vec![UserPart::Text(TextPart {
                id: part_id,
                text: task,
            })],
        })]),
    })
}

fn print_configuration(
    provider: &ProviderConfig,
    options: &RecordOptions,
    model: &dyn ModelService,
) {
    let retry = options.retry_policy.as_ref().map_or_else(
        || "disabled".to_owned(),
        |policy| {
            format!(
                "enabled (max attempts {}, reasons {}, max Retry-After {} ms)",
                policy.delays.len().saturating_add(1),
                policy.retry_on.len(),
                policy.max_retry_after.as_millis()
            )
        },
    );
    eprintln!("Reliability Demo record 配置（credential 已配置但不会显示）：");
    eprintln!("  Provider: deepseek");
    eprintln!("  Endpoint: {}", provider.endpoint);
    eprintln!("  Model: {}", provider.model);
    eprintln!("  Context window: {}", model.context_window_tokens());
    eprintln!("  Trace directory: {}", options.data_dir.display());
    eprintln!(
        "  Trace queue/max bytes: {}/{}",
        options.collector.queue_capacity, options.collector.max_trace_bytes
    );
    eprintln!("  Retry: {retry}");
    eprintln!("  Agent budget: 4 model steps / 1 tool call");
}

async fn print_summary(
    summary: &TraceSummary,
    outcome: &ExecutionOutcome,
) -> Result<(), RecordCommandError> {
    println!("Record 完成：");
    println!("  Agent outcome: {}", outcome_name(outcome));
    println!("  Trace: {}", summary.path.display());
    println!("  Completeness: {:?}", summary.completeness);
    println!(
        "  Records/file bytes: {}/{}",
        summary.record_count, summary.file_bytes
    );
    if let Some(reason) = summary.incomplete_reason {
        println!("  Incomplete reason: {reason:?}");
    }
    if summary.completeness == CollectorCompleteness::Complete {
        let trace = crate::trace::load_complete(&summary.path)
            .await
            .map_err(|error| RecordCommandError::TraceInspection(error.to_string()))?;
        let statistics = statistics(&trace);
        println!(
            "  Logical calls/attempts: {}/{}",
            statistics.logical_calls, statistics.attempts
        );
        println!(
            "  Layer records: provider={}, model={}, agent={}, host={}",
            statistics.provider, statistics.model, statistics.agent, statistics.host
        );
    }
    Ok(())
}

fn outcome_name(outcome: &ExecutionOutcome) -> &'static str {
    match outcome {
        ExecutionOutcome::Completed(_) => "completed",
        ExecutionOutcome::Failed(_) => "failed",
        ExecutionOutcome::Cancelled => "cancelled",
        ExecutionOutcome::CompactionRequired { .. } => "compaction_required",
    }
}

fn record_agent_event(sink: &TraceSink, event: agent_core::AgentEvent) {
    let dropped_events = match &event {
        agent_core::AgentEvent::ExecutionCompleted { dropped_events, .. }
        | agent_core::AgentEvent::ExecutionFailed { dropped_events, .. }
        | agent_core::AgentEvent::ExecutionCancelled { dropped_events }
        | agent_core::AgentEvent::ExecutionCompactionRequired { dropped_events, .. } => {
            *dropped_events
        }
        _ => 0,
    };
    let _ = sink.record(NativeTracePayload::Agent(event));
    if dropped_events > 0 {
        sink.mark_incomplete(IncompleteReason::AgentEventsDropped);
    }
}

#[derive(Default)]
struct TraceStatistics {
    logical_calls: usize,
    attempts: usize,
    provider: usize,
    model: usize,
    agent: usize,
    host: usize,
}

fn statistics(trace: &LoadedTrace) -> TraceStatistics {
    let mut statistics = TraceStatistics::default();
    for record in &trace.records {
        match record.layer {
            TraceLayer::Provider => statistics.provider += 1,
            TraceLayer::Model => statistics.model += 1,
            TraceLayer::Agent => statistics.agent += 1,
            TraceLayer::Host => statistics.host += 1,
        }
        if matches!(record.payload, NativeTracePayload::ModelRequest(_)) {
            statistics.logical_calls += 1;
        }
        if matches!(
            record.payload,
            NativeTracePayload::ProviderWire(
                agent_openai_compatible::ProviderWireEvent::Request { .. }
            )
        ) {
            statistics.attempts += 1;
        }
    }
    statistics
}

/// 在 ModelService 边界记录完整请求、建立结果与调用方实际消费的所有事件。
struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    sink: TraceSink,
    next_correlation: AtomicU64,
}

impl ObservedModelService {
    fn new(inner: Arc<dyn ModelService>, sink: TraceSink) -> Self {
        Self {
            inner,
            sink,
            next_correlation: AtomicU64::new(1),
        }
    }
}

impl ModelService for ObservedModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let correlation = self.next_correlation.fetch_add(1, Ordering::Relaxed);
        let trace = TraceContext::new(format!("model-call-{correlation}"));
        let _ = self.sink.record_with_trace(
            Some(&trace),
            NativeTracePayload::ModelRequest(request.clone()),
        );
        Box::pin(async move {
            let result = self
                .inner
                .stream(
                    request,
                    ModelCallContext {
                        cancellation: context.cancellation,
                        trace: Some(trace.clone()),
                        prepared_images: context.prepared_images,
                    },
                )
                .await;
            match result {
                Err(error) => {
                    let _ = self.sink.record_with_trace(
                        Some(&trace),
                        NativeTracePayload::ModelCall(ModelCallEvent::EstablishmentFailed {
                            error: error.clone(),
                        }),
                    );
                    Err(error)
                }
                Ok(stream) => {
                    let _ = self.sink.record_with_trace(
                        Some(&trace),
                        NativeTracePayload::ModelCall(ModelCallEvent::StreamEstablished),
                    );
                    let sink = self.sink.clone();
                    let observed = stream.map(move |event| {
                        let _ = sink.record_with_trace(
                            Some(&trace),
                            NativeTracePayload::ModelEvent(event.clone()),
                        );
                        event
                    });
                    Ok(Box::pin(observed) as ModelEventStream)
                }
            }
        })
    }
}

/// Demo 私有两阶段 Journal；只验证 Core 的 begin/commit 边界，不落产品会话。
struct DemoJournal {
    sink: TraceSink,
    next_receipt: AtomicU64,
    pending: Mutex<BTreeMap<String, agent_types::AssistantMessage>>,
}

impl DemoJournal {
    fn new(sink: TraceSink) -> Self {
        Self {
            sink,
            next_receipt: AtomicU64::new(1),
            pending: Mutex::new(BTreeMap::new()),
        }
    }
}

impl ExecutionRecorder for DemoJournal {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: agent_types::AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            let receipt_value = format!(
                "demo-exchange-{}",
                self.next_receipt.fetch_add(1, Ordering::Relaxed)
            );
            let receipt = ExchangeReceipt::new(receipt_value.clone())?;
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(receipt_value, assistant);
            let _ = self.sink.record(NativeTracePayload::Host(
                DemoHostEvent::JournalBeginFinished { succeeded: true },
            ));
            Ok(receipt)
        })
    }

    fn mark_tool_execution_started<'a>(
        &'a self,
        _receipt: &'a ExchangeReceipt,
        _call_id: &'a agent_types::ToolCallId,
    ) -> RecordFuture<'a, ()> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        _results: Vec<agent_types::ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let existed = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(receipt.as_str())
                .is_some();
            let _ = self.sink.record(NativeTracePayload::Host(
                DemoHostEvent::JournalCommitFinished { succeeded: existed },
            ));
            if existed {
                Ok(())
            } else {
                Err(RecordError {
                    message: "the Demo Journal does not contain this pending exchange".into(),
                })
            }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct ReliabilityProbeInput {
    /// 不包含用户正文的简短任务类别。
    label: String,
}

#[derive(Debug, Serialize)]
struct ReliabilityProbeOutput {
    acknowledged: bool,
}

/// 固定、确定性且不访问文件、Shell、网络或记忆的验证工具。
struct ReliabilityProbe;

impl Tool for ReliabilityProbe {
    type Input = ReliabilityProbeInput;
    type ResolvedInput = ReliabilityProbeInput;
    type Output = ReliabilityProbeOutput;

    fn name(&self) -> ToolName {
        ToolName::new("reliability_probe").expect("the fixed tool name is valid")
    }

    fn description(&self) -> String {
        "Record one deterministic, side-effect-free probe before the final answer. Provide only a short task category, never the user's full text.".into()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.label.trim().is_empty() || input.label.chars().count() > 64 {
            return Err(ToolError::invalid_input(
                "label must contain 1 to 64 characters",
            ));
        }
        Ok(ToolResolution::general(input))
    }

    fn execute<'a>(
        &'a self,
        _input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(std::future::ready(Ok(ReliabilityProbeOutput {
            acknowledged: true,
        })))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, time::Duration};

    use agent_model::{ModelEvent, ModelRetryReason};
    use agent_openai_compatible::{ProviderWireEvent, RecordedWireRequest};
    use agent_testkit::{ModelScript, ScriptedModelService, message_events};
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, ModelIdentity, ProviderId, ToolCall,
        ToolCallId,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::{
        collector::CollectorConfig,
        replay::{
            RecordedModelOutcome,
            test_support::{model_trace, request, text_events},
        },
        trace::TraceRecord,
    };

    struct ImmediateModel {
        capabilities: ModelCapabilities,
        events: Vec<ModelEvent>,
    }

    #[test]
    fn record_uses_explicit_deepseek_thinking_request_config() {
        let config = deepseek_model_request_config();
        assert_eq!(config.reasoning, Some(ReasoningConfig { effort: None }));
        assert_eq!(
            config.provider_options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
    }

    impl ModelService for ImmediateModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            4096
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            let events = self.events.clone();
            Box::pin(
                async move { Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream) },
            )
        }
    }

    #[tokio::test]
    async fn observed_model_records_one_complete_logical_call() {
        let directory = TempDir::new().expect("temp directory");
        let fixture = model_trace(RecordedModelOutcome::Established, text_events());
        let collector = TraceCollector::start(
            directory.path(),
            fixture.started.provider,
            CollectorConfig::default(),
        )
        .await
        .expect("start collector");
        let service = ObservedModelService::new(
            Arc::new(ImmediateModel {
                capabilities: ModelCapabilities {
                    reasoning: false,
                    image_input: false,
                    tool_calls: true,
                    multimodal_tool_result: false,
                    tool_choice: agent_model::ToolChoiceCapabilities::all(),
                    streaming: true,
                },
                events: text_events(),
            }),
            collector.sink(),
        );
        let mut stream = service
            .stream(request(), ModelCallContext::default())
            .await
            .expect("establish stream");
        while stream.next().await.is_some() {}
        let summary = collector.finish().await.expect("finish collector");
        let trace = crate::trace::load_complete(&summary.path)
            .await
            .expect("load complete trace");
        let calls = crate::replay::recorded_model_calls(&trace).expect("extract call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].correlation_id, "model-call-1");
        assert_eq!(calls[0].events, text_events());
    }

    #[tokio::test]
    async fn concurrent_observed_calls_keep_correlations_and_events_isolated() {
        async fn collect_call(service: &ObservedModelService) -> Vec<ModelEvent> {
            let mut stream = service
                .stream(request(), ModelCallContext::default())
                .await
                .expect("establish stream");
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event);
            }
            events
        }

        let directory = TempDir::new().expect("temp directory");
        let fixture = model_trace(RecordedModelOutcome::Established, text_events());
        let collector = TraceCollector::start(
            directory.path(),
            fixture.started.provider,
            CollectorConfig::default(),
        )
        .await
        .expect("start collector");
        let service = ObservedModelService::new(
            Arc::new(ImmediateModel {
                capabilities: ModelCapabilities {
                    reasoning: false,
                    image_input: false,
                    tool_calls: true,
                    multimodal_tool_result: false,
                    tool_choice: agent_model::ToolChoiceCapabilities::all(),
                    streaming: true,
                },
                events: text_events(),
            }),
            collector.sink(),
        );

        let (first, second) = tokio::join!(collect_call(&service), collect_call(&service));
        assert_eq!(first, text_events());
        assert_eq!(second, text_events());
        let summary = collector.finish().await.expect("finish collector");
        let trace = crate::trace::load_complete(&summary.path)
            .await
            .expect("load complete trace");
        let calls = crate::replay::recorded_model_calls(&trace).expect("extract calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls
                .iter()
                .map(|call| call.correlation_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["model-call-1", "model-call-2"])
        );
        assert!(calls.iter().all(|call| call.events == text_events()));
    }

    #[tokio::test]
    async fn trace_failure_during_stream_does_not_change_cancellation_terminal() {
        let directory = TempDir::new().expect("temp directory");
        let fixture = model_trace(RecordedModelOutcome::Established, text_events());
        let collector = TraceCollector::start(
            directory.path(),
            fixture.started.provider,
            CollectorConfig::default(),
        )
        .await
        .expect("start collector");
        let sink = collector.sink();
        let scripted: Arc<dyn ModelService> = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: false,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            4096,
            [ModelScript::Events(text_events())],
        ));
        let service = ObservedModelService::new(scripted, sink.clone());
        let cancellation = CancellationToken::new();
        let mut stream = service
            .stream(request(), ModelCallContext::new(cancellation.clone()))
            .await
            .expect("establish stream");
        assert!(matches!(
            stream.next().await,
            Some(ModelEvent::TurnStarted { .. })
        ));

        // Collector 边界已经覆盖真实 writer failure；这里组合等价的 Incomplete 状态、
        // 已建立模型流与取消，验证 Trace 失败不会改写模型终态。
        sink.mark_incomplete(IncompleteReason::WriteFailed);
        cancellation.cancel();
        assert_eq!(
            stream.next().await,
            Some(ModelEvent::TurnFailed {
                error: agent_model::ModelError::Cancelled,
            })
        );
        assert!(stream.next().await.is_none());

        let summary = collector.finish().await.expect("finish collector");
        assert_eq!(summary.completeness, CollectorCompleteness::Incomplete);
        assert_eq!(
            summary.incomplete_reason,
            Some(IncompleteReason::WriteFailed)
        );
    }

    #[tokio::test]
    async fn agent_loop_records_tool_journal_and_second_model_call() {
        let directory = TempDir::new().expect("temp directory");
        let fixture = model_trace(RecordedModelOutcome::Established, text_events());
        let collector = TraceCollector::start(
            directory.path(),
            fixture.started.provider,
            CollectorConfig::default(),
        )
        .await
        .expect("start collector");
        let sink = collector.sink();
        let identity = ModelIdentity::new(
            ProviderId::new("deepseek").expect("valid provider"),
            "fixture-model",
        );
        let tool_turn = AssistantMessage {
            id: MessageId::new("tool-turn").expect("valid id"),
            model: identity.clone(),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("probe-call").expect("valid id"),
                name: ToolName::new("reliability_probe").expect("valid name"),
                arguments: serde_json::json!({"label": "test"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        let final_turn = AssistantMessage {
            id: MessageId::new("final-turn").expect("valid id"),
            model: identity,
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new("final-text").expect("valid id"),
                text: "done".into(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        let scripted: Arc<dyn ModelService> = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: false,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::all(),
                streaming: true,
            },
            4096,
            [
                ModelScript::Events(message_events(&tool_turn)),
                ModelScript::Events(message_events(&final_turn)),
            ],
        ));
        let observed: Arc<dyn ModelService> =
            Arc::new(ObservedModelService::new(scripted, sink.clone()));
        let journal = Arc::new(DemoJournal::new(sink.clone()));
        let _ = sink.record(NativeTracePayload::Host(
            DemoHostEvent::AgentExecutionStarted,
        ));
        let AgentExecution {
            mut events,
            completion,
            control: _,
        } = AgentExecution::start(
            ExecutionSpec {
                system_prompt: SystemPromptSnapshot::new(vec![SYSTEM_PROMPT.into()]),
                model: observed,
                context_window: Arc::new(
                    ContextWindowEvaluator::new(0.8).expect("valid threshold"),
                ),
                tools: tool_snapshot().expect("fixed tool"),
                model_request: agent_core::ModelRequestConfig::default(),
                budget: ExecutionBudget {
                    max_steps: Some(4),
                    max_tool_calls: Some(1),
                },
                guardrails: None,
            },
            execution_input("test task".into()).expect("valid input"),
            ExecutionContext {
                cancellation: CancellationToken::new(),
                recorder: journal.clone(),
                authorizer: Arc::new(AllowAllAuthorizer),
            },
        );
        while events.next().await.is_some_and(|event| {
            let terminal = event.is_terminal();
            let _ = sink.record(NativeTracePayload::Agent(event));
            !terminal
        }) {}
        let outcome = completion.await;
        assert!(matches!(outcome, ExecutionOutcome::Completed(_)));
        let _ = sink.record(NativeTracePayload::Host(
            DemoHostEvent::AgentExecutionFinished { outcome },
        ));
        assert!(journal.pending.lock().expect("journal lock").is_empty());
        let summary = collector.finish().await.expect("finish collector");
        let trace = crate::trace::load_complete(&summary.path)
            .await
            .expect("load complete trace");
        assert_eq!(
            crate::replay::recorded_model_calls(&trace)
                .expect("extract calls")
                .len(),
            2
        );
        assert!(trace.records.iter().any(|record| matches!(
            record.payload,
            NativeTracePayload::Host(DemoHostEvent::JournalBeginFinished { succeeded: true })
        )));
        assert!(trace.records.iter().any(|record| matches!(
            record.payload,
            NativeTracePayload::Host(DemoHostEvent::JournalCommitFinished { succeeded: true })
        )));
        assert!(trace.records.iter().any(|record| matches!(
            record.payload,
            NativeTracePayload::Agent(agent_core::AgentEvent::ToolStarted { .. })
        )));
    }

    #[tokio::test]
    async fn dropped_agent_events_make_the_trace_incomplete() {
        let directory = TempDir::new().expect("temp directory");
        let fixture = model_trace(RecordedModelOutcome::Established, text_events());
        let collector = TraceCollector::start(
            directory.path(),
            fixture.started.provider,
            CollectorConfig::default(),
        )
        .await
        .expect("start collector");
        record_agent_event(
            &collector.sink(),
            agent_core::AgentEvent::ExecutionCancelled { dropped_events: 1 },
        );
        let summary = collector.finish().await.expect("finish collector");
        assert_eq!(summary.completeness, CollectorCompleteness::Incomplete);
        assert_eq!(
            summary.incomplete_reason,
            Some(IncompleteReason::AgentEventsDropped)
        );
    }

    #[test]
    fn statistics_counts_requests_as_attempts_and_preserves_layers() {
        let mut trace = model_trace(RecordedModelOutcome::Established, text_events());
        let sequence = u64::try_from(trace.records.len() + 1).expect("small fixture");
        trace.records.push(TraceRecord {
            sequence,
            observed_at_ms: sequence + 1,
            layer: TraceLayer::Provider,
            correlation_id: Some(crate::replay::test_support::CORRELATION.into()),
            attempt: None,
            payload: NativeTracePayload::ProviderWire(ProviderWireEvent::Request {
                trace: Some(TraceContext::new(crate::replay::test_support::CORRELATION)),
                request: RecordedWireRequest {
                    method: "POST".into(),
                    url: "https://example.invalid/chat/completions".into(),
                    headers: vec![],
                    body: vec![],
                },
            }),
        });
        let statistics = statistics(&trace);
        assert_eq!(statistics.logical_calls, 1);
        assert_eq!(statistics.attempts, 1);
        assert!(statistics.provider > 0);
        assert!(statistics.model > 0);
    }

    #[test]
    fn explicit_retry_policy_shape_has_no_hidden_attempts() {
        let policy = ModelRetryPolicy::new(
            BTreeSet::from([ModelRetryReason::Timeout]),
            vec![Duration::from_millis(10), Duration::from_millis(20)],
            Duration::from_millis(100),
        );
        assert_eq!(policy.delays.len() + 1, 3);
    }

    #[test]
    fn provider_configuration_fails_without_credential_and_validates_context_window() {
        assert!(matches!(
            provider_config_from(|_| None),
            Err(RecordCommandError::MissingCredential)
        ));
        let invalid = provider_config_from(|name| match name {
            "DEEPSEEK_API_KEY" => Some("secret-never-rendered".into()),
            "DEEPSEEK_CONTEXT_WINDOW_TOKENS" => Some("0".into()),
            _ => None,
        });
        assert!(matches!(
            invalid,
            Err(RecordCommandError::InvalidContextWindow)
        ));
        let defaults = provider_config_from(|name| {
            (name == "DEEPSEEK_API_KEY").then(|| "secret-never-rendered".into())
        })
        .expect("valid defaults");
        assert_eq!(defaults.endpoint, DEFAULT_BASE_URL);
        assert_eq!(defaults.model, DEFAULT_MODEL);
        assert_eq!(
            defaults.context_window_tokens,
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
    }
}
