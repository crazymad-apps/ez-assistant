//! Model, Agent, and Runtime debug event forwarding.
//!
//! The Harness owns this integration boundary. Provider and Core implementations
//! remain unaware of the viewer, while every event for one Run shares a client,
//! correlation ID, sequence, queue, and failure-muting state.

use std::{sync::Arc, time::Instant};

use agent_context::ContextWindowEvaluation;
use agent_core::{AgentEvent, ExecutionOutcome};
use agent_model::{
    ModelCallContext, ModelCapabilities, ModelEventStream, ModelRequest, ModelService,
    ModelStreamFuture,
};
use agent_types::UserMessage;
use debug_viewer::{DebugChannel, DebugClient, DebugPayload};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{
    cli::DebugLayerSelection,
    context::{HarnessCompactionOutcome, HarnessCompactionReport},
    runtime::{HarnessRunId, RuntimeSnapshot},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DebugRoute {
    provider: bool,
    agent_runtime: bool,
}

impl From<DebugLayerSelection> for DebugRoute {
    fn from(selection: DebugLayerSelection) -> Self {
        match selection {
            DebugLayerSelection::Provider => Self {
                provider: true,
                agent_runtime: false,
            },
            DebugLayerSelection::Agent => Self {
                provider: false,
                agent_runtime: true,
            },
            DebugLayerSelection::Both => Self {
                provider: true,
                agent_runtime: true,
            },
        }
    }
}

/// Per-Run debug integration. All clones share one [`DebugClient`].
#[derive(Clone)]
pub(crate) struct RunDebug {
    client: Arc<DebugClient>,
    route: DebugRoute,
    session_id: String,
    run_id: String,
    endpoint: String,
    configured_model: String,
}

impl RunDebug {
    pub(crate) fn for_run(
        base_url: Option<&str>,
        selection: DebugLayerSelection,
        snapshot: &RuntimeSnapshot,
        run_id: &HarnessRunId,
        correlation_id: String,
        endpoint: &str,
        configured_model: &str,
    ) -> Option<Self> {
        let base_url = base_url.filter(|value| !value.trim().is_empty())?;
        Some(Self::new(
            base_url,
            selection,
            snapshot.session_id.to_string(),
            run_id.to_string(),
            correlation_id,
            endpoint,
            configured_model,
        ))
    }

    pub(crate) fn for_context_operation(
        base_url: Option<&str>,
        selection: DebugLayerSelection,
        snapshot: &RuntimeSnapshot,
        endpoint: &str,
        configured_model: &str,
    ) -> Option<Self> {
        let base_url = base_url.filter(|value| !value.trim().is_empty())?;
        let session_id = snapshot.session_id.to_string();
        Some(Self::new(
            base_url,
            selection,
            session_id.clone(),
            "context".to_owned(),
            format!("{session_id}/context"),
            endpoint,
            configured_model,
        ))
    }

    fn new(
        base_url: &str,
        selection: DebugLayerSelection,
        session_id: String,
        run_id: String,
        correlation_id: String,
        endpoint: &str,
        configured_model: &str,
    ) -> Self {
        Self {
            client: Arc::new(DebugClient::new(base_url).with_correlation_id(correlation_id)),
            route: selection.into(),
            session_id,
            run_id,
            endpoint: endpoint.to_owned(),
            configured_model: configured_model.to_owned(),
        }
    }

    /// Apply provider observation only when the selected route includes it.
    pub(crate) fn observe_model(&self, inner: Arc<dyn ModelService>) -> Arc<dyn ModelService> {
        if !self.route.provider {
            return inner;
        }
        Arc::new(ObservedModelService {
            inner,
            debug: Arc::clone(&self.client),
            endpoint: self.endpoint.clone(),
            configured_model: self.configured_model.clone(),
        })
    }

    pub(crate) fn post_agent(&self, event: &AgentEvent) {
        if self.route.agent_runtime {
            self.client.post_on(
                DebugChannel::Agent,
                DebugPayload::AgentEvent {
                    event: event.clone(),
                },
            );
        }
    }

    pub(crate) fn post_run_started(&self, snapshot: &RuntimeSnapshot) {
        self.post_runtime_snapshot("run_started", snapshot, None);
    }

    pub(crate) fn post_user_message(&self, message: &UserMessage) {
        self.post_runtime(
            "user_message_appended",
            json!({
                "session_id": self.session_id,
                "run_id": self.run_id,
                "message": message,
            }),
        );
    }

    pub(crate) fn post_cancel_requested(&self) {
        self.post_runtime(
            "cancel_requested",
            json!({
                "session_id": self.session_id,
                "run_id": self.run_id,
            }),
        );
    }

    pub(crate) fn post_context_preflight(&self, evaluation: &ContextWindowEvaluation) {
        self.post_runtime(
            "context_window_evaluated",
            json!({
                "session_id": self.session_id,
                "run_id": self.run_id,
                "evaluation": evaluation,
            }),
        );
    }

    pub(crate) fn post_compaction_report(
        &self,
        outcome: &str,
        report: &HarnessCompactionReport,
        checkpoint_count: usize,
    ) {
        self.post_runtime(
            "context_compaction_finished",
            json!({
                "session_id": self.session_id,
                "run_id": self.run_id,
                "outcome": outcome,
                "report": report,
                "checkpoint_count": checkpoint_count,
            }),
        );
    }

    pub(crate) fn post_user_compaction_outcome(
        &self,
        outcome: &HarnessCompactionOutcome,
        checkpoint_count: usize,
    ) {
        match outcome {
            HarnessCompactionOutcome::Compacted { report, .. } => {
                self.post_compaction_report("compacted", report, checkpoint_count);
            }
            HarnessCompactionOutcome::NoOp { report } => {
                self.post_compaction_report("no_op", report, checkpoint_count);
            }
        }
    }

    pub(crate) fn post_compaction_queued(&self) {
        self.post_runtime(
            "user_compaction_queued",
            json!({
                "session_id": self.session_id,
                "run_id": self.run_id,
            }),
        );
    }

    pub(crate) fn post_continuation_started(
        &self,
        previous_run_id: &HarnessRunId,
        next_run_id: &HarnessRunId,
    ) {
        self.post_runtime(
            "continuation_started",
            json!({
                "session_id": self.session_id,
                "previous_run_id": previous_run_id.to_string(),
                "run_id": next_run_id.to_string(),
            }),
        );
    }

    pub(crate) fn post_run_finished(&self, snapshot: &RuntimeSnapshot, outcome: &ExecutionOutcome) {
        let (name, error) = match outcome {
            ExecutionOutcome::Completed(_) => ("run_completed", None),
            ExecutionOutcome::Failed(error) => ("run_failed", Some(error.to_string())),
            ExecutionOutcome::Cancelled => ("run_cancelled", None),
            ExecutionOutcome::CompactionRequired { reason, step, .. } => (
                "run_compaction_required",
                Some(format!("{reason:?} at step {step}")),
            ),
        };
        self.post_runtime_snapshot(name, snapshot, error);
        self.post_runtime(
            "journal_updated",
            json!({
                "session_id": snapshot.session_id.to_string(),
                "run_id": self.run_id,
                "completed_messages": snapshot.completed_messages,
                "roles": snapshot
                    .roles
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "pending_count": snapshot.pending.len(),
                "checkpoint_count": snapshot.checkpoint_count,
                "effective_roles": snapshot
                    .effective_roles
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "automatic_compactions": snapshot.automatic_compactions,
                "max_automatic_compactions": snapshot.max_automatic_compactions,
                "user_compaction_queued": snapshot.user_compaction_queued,
            }),
        );
    }

    fn post_runtime_snapshot(&self, name: &str, snapshot: &RuntimeSnapshot, error: Option<String>) {
        let run = snapshot.run.as_ref();
        self.post_runtime(
            name,
            json!({
                "session_id": snapshot.session_id.to_string(),
                "run_id": self.run_id,
                "status": run.map(|run| run.status.to_string()),
                "created_at_ms": run.and_then(|run| run.created_at_ms).map(ms_u64),
                "started_at_ms": run.and_then(|run| run.started_at_ms).map(ms_u64),
                "finished_at_ms": run.and_then(|run| run.finished_at_ms).map(ms_u64),
                "elapsed_ms": run
                    .and_then(|run| run.elapsed)
                    .map(|elapsed| elapsed.as_millis())
                    .map(ms_u64),
                "event_count": run.map(|run| run.event_count),
                "dropped_events": run.map(|run| run.dropped_events),
                "error": error,
            }),
        );
    }

    fn post_runtime(&self, name: &str, data: Value) {
        if self.route.agent_runtime {
            self.client.post_on(
                DebugChannel::Runtime,
                DebugPayload::RuntimeEvent {
                    name: name.to_owned(),
                    data,
                },
            );
        }
    }
}

fn ms_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    debug: Arc<DebugClient>,
    endpoint: String,
    configured_model: String,
}

impl ModelService for ObservedModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let debug = Arc::clone(&self.debug);
        let endpoint = self.endpoint.clone();
        let configured_model = self.configured_model.clone();
        Box::pin(async move {
            let message_count = request.conversation.messages.len() as u32;
            let tool_count = request.tools.len() as u32;
            debug.post(DebugPayload::TurnRequested {
                request: request.clone(),
            });
            let started = Instant::now();
            let stream = match self.inner.stream(request, context).await {
                Ok(stream) => stream,
                Err(error) => {
                    debug.post(DebugPayload::EstablishmentFailed {
                        error: error.to_string(),
                    });
                    return Err(error);
                }
            };
            debug.post(DebugPayload::TurnEstablished {
                model: configured_model,
                endpoint,
                message_count,
                tool_count,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
            let observed = stream.inspect(move |event| {
                debug.post(DebugPayload::ModelEvent {
                    event: event.clone(),
                });
            });
            Ok(Box::pin(observed) as ModelEventStream)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use agent_context::{ContextWindowDecision, StrategyReport};
    use agent_core::{AgentExecution, ExecutionBudget, ExecutionSpec};
    use agent_model::{
        GenerationConfig, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
        ProviderOptions,
    };
    use agent_testkit::{ModelScript, ScriptedModelService, message_events};
    use agent_tools::ToolRegistry;
    use agent_types::{
        AssistantMessage, AssistantPart, ConversationSnapshot, FinishReason, MessageId,
        ModelIdentity, PartId, ProviderId, TextPart, ToolChoice, UserMessage, UserPart,
    };
    use debug_viewer::BroadcastMessage;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::runtime::HarnessRuntime;

    const TEST_CONTEXT_WINDOW_TOKENS: u64 = 128_000;

    async fn start_viewer() -> (String, mpsc::Receiver<BroadcastMessage>) {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind viewer");
        let address = listener.local_addr().expect("viewer address");
        tokio::spawn(async move {
            axum::serve(listener, debug_viewer::router())
                .await
                .expect("serve viewer");
        });

        let (messages_tx, messages_rx) = mpsc::channel(64);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(async move {
            let response = reqwest::Client::new()
                .get(format!("http://{address}/events"))
                .send()
                .await
                .expect("subscribe viewer SSE");
            let _ = ready_tx.send(());
            let mut bytes = response.bytes_stream();
            let mut buffer = String::new();
            while let Some(chunk) = bytes.next().await {
                buffer.push_str(
                    std::str::from_utf8(&chunk.expect("read SSE chunk")).expect("SSE is UTF-8"),
                );
                while let Some(boundary) = buffer.find("\n\n") {
                    let frame = buffer[..boundary].to_owned();
                    buffer.drain(..boundary + 2);
                    let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: "))
                    else {
                        continue;
                    };
                    let message =
                        serde_json::from_str(data).expect("decode viewer broadcast message");
                    if messages_tx.send(message).await.is_err() {
                        return;
                    }
                }
            }
        });
        ready_rx.await.expect("SSE subscription ready");
        (format!("http://{address}"), messages_rx)
    }

    async fn receive_messages(
        receiver: &mut mpsc::Receiver<BroadcastMessage>,
        count: usize,
    ) -> Vec<BroadcastMessage> {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut messages = Vec::with_capacity(count);
            while messages.len() < count {
                messages.push(receiver.recv().await.expect("viewer message"));
            }
            messages
        })
        .await
        .expect("viewer messages timed out")
    }

    fn capabilities() -> ModelCapabilities {
        ModelCapabilities {
            reasoning: false,
            image_input: false,
            tool_calls: true,
            multimodal_tool_result: false,
            tool_choice: agent_model::ToolChoiceCapabilities::all(),
            streaming: true,
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            system: agent_model::SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }

    fn assistant() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant_debug").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("provider id"),
                "scripted-model",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new("assistant_debug_text").expect("part id"),
                text: "done".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn user() -> UserMessage {
        UserMessage {
            id: MessageId::new("user_debug").expect("message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("user_debug_text").expect("part id"),
                text: "hello runtime".to_owned(),
            })],
        }
    }

    fn run_debug(
        viewer_url: Option<&str>,
        selection: DebugLayerSelection,
        runtime: &mut HarnessRuntime,
    ) -> (crate::runtime::PreparedRun, Option<RunDebug>) {
        let prepared = runtime.prepare_run("debug this").expect("prepare run");
        let snapshot = runtime.snapshot().expect("runtime snapshot");
        let debug = RunDebug::for_run(
            viewer_url,
            selection,
            &snapshot,
            &prepared.run_id,
            runtime.correlation_id(&prepared.run_id),
            "http://provider.test",
            "scripted-model",
        );
        (prepared, debug)
    }

    #[test]
    fn layer_selection_maps_to_exact_channels() {
        assert_eq!(
            DebugRoute::from(DebugLayerSelection::Provider),
            DebugRoute {
                provider: true,
                agent_runtime: false,
            }
        );
        assert_eq!(
            DebugRoute::from(DebugLayerSelection::Agent),
            DebugRoute {
                provider: false,
                agent_runtime: true,
            }
        );
        assert_eq!(
            DebugRoute::from(DebugLayerSelection::Both),
            DebugRoute {
                provider: true,
                agent_runtime: true,
            }
        );
    }

    #[test]
    fn missing_debug_url_creates_neither_client_nor_decorator() {
        let mut runtime = HarnessRuntime::for_scenario("no_debug");
        let (_, debug) = run_debug(None, DebugLayerSelection::Both, &mut runtime);
        assert!(debug.is_none());
    }

    #[tokio::test]
    async fn observed_model_is_transparent_for_request_events_and_establishment_error() {
        let (viewer_url, mut messages) = start_viewer().await;
        let expected_events = message_events(&assistant());
        let inner = Arc::new(ScriptedModelService::new(
            capabilities(),
            TEST_CONTEXT_WINDOW_TOKENS,
            [
                ModelScript::Events(expected_events.clone()),
                ModelScript::FailEstablishment(ModelError::Auth("denied".to_owned())),
            ],
        ));
        let debug =
            Arc::new(DebugClient::new(&viewer_url).with_correlation_id("session_test/run_1"));
        let observed = ObservedModelService {
            inner: inner.clone(),
            debug,
            endpoint: "http://provider.test".to_owned(),
            configured_model: "scripted-model".to_owned(),
        };

        assert_eq!(observed.capabilities(), &capabilities());
        let expected_request = request();
        let stream = observed
            .stream(expected_request.clone(), ModelCallContext::default())
            .await
            .expect("establish observed stream");
        let actual_events = stream.collect::<Vec<_>>().await;
        assert_eq!(actual_events, expected_events);

        let error = match observed
            .stream(request(), ModelCallContext::default())
            .await
        {
            Ok(_) => panic!("second call must fail before establishment"),
            Err(error) => error,
        };
        assert_eq!(error, ModelError::Auth("denied".to_owned()));
        assert_eq!(
            inner.take_requests(),
            vec![expected_request, request()],
            "decorator must forward requests without modification"
        );

        let received = receive_messages(&mut messages, expected_events.len() + 4).await;
        assert!(received.iter().all(|message| {
            message.envelope.ch == DebugChannel::Llm
                && message.envelope.correlation_id.as_deref() == Some("session_test/run_1")
        }));
        assert_eq!(
            received
                .iter()
                .map(|message| message.envelope.seq)
                .collect::<Vec<_>>(),
            (0..received.len() as u64).collect::<Vec<_>>()
        );
        assert!(matches!(
            received.first().map(|message| &message.envelope.payload),
            Some(DebugPayload::TurnRequested { .. })
        ));
        assert!(matches!(
            received.last().map(|message| &message.envelope.payload),
            Some(DebugPayload::EstablishmentFailed { .. })
        ));
    }

    #[tokio::test]
    async fn observed_model_preserves_cancellation_and_terminal_event() {
        let (viewer_url, mut messages) = start_viewer().await;
        let expected_events = message_events(&assistant());
        let inner = Arc::new(ScriptedModelService::new(
            capabilities(),
            TEST_CONTEXT_WINDOW_TOKENS,
            [
                ModelScript::Events(expected_events),
                ModelScript::Events(vec![
                    ModelEvent::TurnStarted {
                        message_id: MessageId::new("cancelled").expect("message id"),
                        model: ModelIdentity::new(
                            ProviderId::new("scripted").expect("provider id"),
                            "scripted-model",
                        ),
                    },
                    ModelEvent::TurnFinished {
                        message: assistant(),
                    },
                ]),
            ],
        ));
        let observed = ObservedModelService {
            inner,
            debug: Arc::new(
                DebugClient::new(&viewer_url).with_correlation_id("session_test/run_2"),
            ),
            endpoint: "http://provider.test".to_owned(),
            configured_model: "scripted-model".to_owned(),
        };

        let cancellation = CancellationToken::new();
        let stream = observed
            .stream(request(), ModelCallContext::new(cancellation.clone()))
            .await
            .expect("establish stream");
        cancellation.cancel();
        assert_eq!(
            stream.collect::<Vec<_>>().await,
            vec![ModelEvent::TurnFailed {
                error: ModelError::Cancelled,
            }]
        );

        let cancelled_before = CancellationToken::new();
        cancelled_before.cancel();
        let error = match observed
            .stream(request(), ModelCallContext::new(cancelled_before))
            .await
        {
            Ok(_) => panic!("pre-cancelled call must fail establishment"),
            Err(error) => error,
        };
        assert_eq!(error, ModelError::Cancelled);

        let received = receive_messages(&mut messages, 5).await;
        assert!(matches!(
            received[2].envelope.payload,
            DebugPayload::ModelEvent {
                event: ModelEvent::TurnFailed {
                    error: ModelError::Cancelled
                }
            }
        ));
        assert!(matches!(
            received[4].envelope.payload,
            DebugPayload::EstablishmentFailed { .. }
        ));
    }

    #[tokio::test]
    async fn harness_posts_three_channels_with_shared_correlation_and_sequence() {
        let (viewer_url, mut messages) = start_viewer().await;
        let mut runtime = HarnessRuntime::for_scenario("three_channels");
        let (prepared, debug) =
            run_debug(Some(&viewer_url), DebugLayerSelection::Both, &mut runtime);
        let debug = debug.expect("debug enabled");
        debug
            .client
            .post(DebugPayload::TurnRequested { request: request() });
        debug.post_agent(&AgentEvent::ExecutionStarted);
        runtime
            .mark_running(&prepared.run_id)
            .expect("mark run running");
        debug.post_run_started(&runtime.snapshot().expect("snapshot"));

        let received = receive_messages(&mut messages, 3).await;
        assert_eq!(
            received
                .iter()
                .map(|message| message.envelope.ch)
                .collect::<Vec<_>>(),
            vec![
                DebugChannel::Llm,
                DebugChannel::Agent,
                DebugChannel::Runtime
            ]
        );
        assert_eq!(
            received
                .iter()
                .map(|message| message.envelope.seq)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(received.iter().all(|message| {
            message.envelope.correlation_id.as_deref()
                == Some("session_verify_three_channels/run_1")
        }));
    }

    #[tokio::test]
    async fn selection_does_not_wrap_or_post_unselected_layers() {
        let (viewer_url, mut messages) = start_viewer().await;
        let mut runtime = HarnessRuntime::for_scenario("agent_only");
        let (prepared, debug) =
            run_debug(Some(&viewer_url), DebugLayerSelection::Agent, &mut runtime);
        let debug = debug.expect("debug enabled");
        let inner: Arc<dyn ModelService> = Arc::new(ScriptedModelService::completing(
            capabilities(),
            TEST_CONTEXT_WINDOW_TOKENS,
            assistant(),
        ));
        let selected = debug.observe_model(Arc::clone(&inner));
        assert!(Arc::ptr_eq(&selected, &inner));
        debug.post_user_message(&user());
        debug.post_agent(&AgentEvent::ExecutionStarted);
        runtime
            .mark_running(&prepared.run_id)
            .expect("mark run running");
        debug.post_run_started(&runtime.snapshot().expect("snapshot"));

        let received = receive_messages(&mut messages, 3).await;
        assert_eq!(
            received
                .iter()
                .map(|message| message.envelope.ch)
                .collect::<Vec<_>>(),
            vec![
                DebugChannel::Runtime,
                DebugChannel::Agent,
                DebugChannel::Runtime
            ]
        );
        assert!(matches!(
            &received[0].envelope.payload,
            DebugPayload::RuntimeEvent { name, data }
                if name == "user_message_appended"
                    && data["message"]["parts"][0]["data"]["text"] == "hello runtime"
        ));
    }

    #[tokio::test]
    async fn provider_selection_posts_only_model_layer() {
        let (viewer_url, mut messages) = start_viewer().await;
        let mut runtime = HarnessRuntime::for_scenario("provider_only");
        let (prepared, debug) = run_debug(
            Some(&viewer_url),
            DebugLayerSelection::Provider,
            &mut runtime,
        );
        let debug = debug.expect("debug enabled");
        debug.post_user_message(&user());
        debug.post_agent(&AgentEvent::ExecutionStarted);
        runtime
            .mark_running(&prepared.run_id)
            .expect("mark run running");
        debug.post_run_started(&runtime.snapshot().expect("snapshot"));

        let expected_events = message_events(&assistant());
        let model = debug.observe_model(Arc::new(ScriptedModelService::new(
            capabilities(),
            TEST_CONTEXT_WINDOW_TOKENS,
            [ModelScript::Events(expected_events.clone())],
        )));
        let stream = model
            .stream(request(), ModelCallContext::default())
            .await
            .expect("establish stream");
        assert_eq!(stream.collect::<Vec<_>>().await, expected_events);

        let received = receive_messages(&mut messages, expected_events.len() + 2).await;
        assert!(
            received
                .iter()
                .all(|message| message.envelope.ch == DebugChannel::Llm)
        );
    }

    #[tokio::test]
    async fn context_runtime_events_are_structured_and_share_the_run_correlation() {
        let (viewer_url, mut messages) = start_viewer().await;
        let mut runtime = HarnessRuntime::for_scenario("context_debug");
        let (prepared, debug) =
            run_debug(Some(&viewer_url), DebugLayerSelection::Agent, &mut runtime);
        let debug = debug.expect("debug enabled");
        let evaluation = ContextWindowEvaluation {
            used_tokens: Some(90),
            context_window_tokens: 100,
            used_ratio: Some(0.9),
            decision: ContextWindowDecision::CompactionRequired,
        };
        let report = HarnessCompactionReport {
            cause: crate::context::HarnessCompactionCause::BeforeRunThreshold,
            strategy: StrategyReport {
                strategy: "rolling_summary_same_model".to_owned(),
                compressed_blocks: 2,
                retained_blocks: 1,
                model: None,
                usage: None,
            },
            trigger: Some(evaluation.clone()),
        };

        debug.post_context_preflight(&evaluation);
        debug.post_compaction_report("compacted", &report, 1);
        debug.post_compaction_queued();
        debug.post_continuation_started(&prepared.run_id, &HarnessRunId::from_sequence(2));

        let received = receive_messages(&mut messages, 4).await;
        assert!(received.iter().all(|message| {
            message.envelope.ch == DebugChannel::Runtime
                && message.envelope.correlation_id.as_deref()
                    == Some("session_verify_context_debug/run_1")
        }));
        assert_eq!(
            received
                .iter()
                .filter_map(|message| match &message.envelope.payload {
                    DebugPayload::RuntimeEvent { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                "context_window_evaluated",
                "context_compaction_finished",
                "user_compaction_queued",
                "continuation_started",
            ]
        );
        let serialized = serde_json::to_string(&received).expect("serialize debug events");
        assert!(!serialized.contains("summary body"));
    }

    #[tokio::test]
    async fn muted_viewer_does_not_change_run_outcome() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve unavailable viewer address");
        let address = listener.local_addr().expect("address");
        drop(listener);

        let mut runtime = HarnessRuntime::for_scenario("muted_viewer");
        let (prepared, debug) = run_debug(
            Some(&format!("http://{address}")),
            DebugLayerSelection::Both,
            &mut runtime,
        );
        let debug = debug.expect("debug enabled");
        let model = debug.observe_model(Arc::new(ScriptedModelService::completing(
            capabilities(),
            TEST_CONTEXT_WINDOW_TOKENS,
            assistant(),
        )));
        let spec = ExecutionSpec {
            system_prompt: agent_model::SystemPromptSnapshot::default(),
            model,
            context_window: Arc::new(
                agent_context::ContextWindowEvaluator::new(0.8).expect("valid test threshold"),
            ),
            tools: ToolRegistry::new().snapshot(),
            model_request: agent_core::ModelRequestConfig::default(),
            budget: ExecutionBudget::default(),
            guardrails: None,
        };
        let (run_id, execution) = runtime
            .start_prepared(spec, prepared)
            .expect("start execution");
        debug.post_run_started(&runtime.snapshot().expect("snapshot"));
        let AgentExecution {
            events,
            completion,
            control: _,
        } = execution;
        let events = events.collect::<Vec<_>>().await;
        let outcome = completion.await;
        for event in &events {
            runtime
                .observe_event(&run_id, event)
                .expect("observe agent event");
            debug.post_agent(event);
        }
        runtime
            .finish_run(&run_id, outcome.clone())
            .expect("finish run");
        debug.post_run_finished(&runtime.snapshot().expect("snapshot"), &outcome);

        assert!(matches!(outcome, ExecutionOutcome::Completed(_)));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !debug.client.is_muted() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed viewer should mute");
    }
}
