//! [`RetryingModelService`] 的确定性边界测试。

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    future::pending,
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_types::{ConversationSnapshot, ToolChoice};
use futures_util::{StreamExt, stream};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    GenerationConfig, ModelAttemptEvent, ModelAttemptObserver, ModelCallContext, ModelCapabilities,
    ModelError, ModelEvent, ModelEventStream, ModelRequest, ModelRetryPolicy, ModelRetryReason,
    ModelService, ModelStreamFuture, ModelTransportErrorKind, ProviderOptions,
    RetryingModelService, SystemPromptSnapshot, TraceContext,
};

#[derive(Clone)]
enum ScriptAction {
    Fail(ModelError),
    Stream(Vec<ModelEvent>),
}

#[derive(Clone, Debug, PartialEq)]
struct RecordedCall {
    request: ModelRequest,
    trace: Option<TraceContext>,
}

struct ScriptedService {
    capabilities: ModelCapabilities,
    context_window_tokens: u64,
    actions: Mutex<VecDeque<ScriptAction>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl ScriptedService {
    fn new(actions: impl IntoIterator<Item = ScriptAction>) -> Self {
        Self {
            capabilities: capabilities(),
            context_window_tokens: 128_000,
            actions: Mutex::new(actions.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("calls lock poisoned").clone()
    }
}

impl ModelService for ScriptedService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.calls
            .lock()
            .expect("calls lock poisoned")
            .push(RecordedCall {
                request,
                trace: context.trace,
            });
        let action = self
            .actions
            .lock()
            .expect("actions lock poisoned")
            .pop_front()
            .expect("test script must contain one action per call");
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            match action {
                ScriptAction::Fail(error) => Err(error),
                ScriptAction::Stream(events) => {
                    Ok(Box::pin(stream::iter(events)) as ModelEventStream)
                }
            }
        })
    }
}

#[derive(Default)]
struct AttemptCollector {
    events: Mutex<Vec<ModelAttemptEvent>>,
    retry_scheduled: Notify,
}

impl AttemptCollector {
    fn events(&self) -> Vec<ModelAttemptEvent> {
        self.events.lock().expect("events lock poisoned").clone()
    }
}

impl ModelAttemptObserver for AttemptCollector {
    fn observe(&self, event: ModelAttemptEvent) {
        if matches!(event, ModelAttemptEvent::RetryScheduled { .. }) {
            self.retry_scheduled.notify_one();
        }
        self.events
            .lock()
            .expect("events lock poisoned")
            .push(event);
    }
}

struct CancellingObserver {
    cancellation: CancellationToken,
    events: Mutex<Vec<ModelAttemptEvent>>,
}

impl ModelAttemptObserver for CancellingObserver {
    fn observe(&self, event: ModelAttemptEvent) {
        if matches!(event, ModelAttemptEvent::RetryScheduled { .. }) {
            self.cancellation.cancel();
        }
        self.events
            .lock()
            .expect("events lock poisoned")
            .push(event);
    }
}

struct DroppingObserver;

impl ModelAttemptObserver for DroppingObserver {
    fn observe(&self, _event: ModelAttemptEvent) {}
}

struct BlockingService {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelService for BlockingService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        128_000
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            pending().await
        })
    }
}

struct PerTraceService {
    capabilities: ModelCapabilities,
    counts: Mutex<HashMap<String, u32>>,
    traces: Mutex<Vec<TraceContext>>,
}

impl PerTraceService {
    fn new() -> Self {
        Self {
            capabilities: capabilities(),
            counts: Mutex::new(HashMap::new()),
            traces: Mutex::new(Vec::new()),
        }
    }

    fn traces(&self) -> Vec<TraceContext> {
        self.traces.lock().expect("traces lock poisoned").clone()
    }
}

impl ModelService for PerTraceService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        128_000
    }

    fn stream(&self, _request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let trace = context.trace.expect("concurrency test requires trace");
        self.traces
            .lock()
            .expect("traces lock poisoned")
            .push(trace.clone());
        let call = {
            let mut counts = self.counts.lock().expect("counts lock poisoned");
            let count = counts.entry(trace.correlation_id).or_default();
            *count += 1;
            *count
        };
        Box::pin(async move {
            if call == 1 {
                Err(connection_error("first attempt"))
            } else {
                Ok(Box::pin(stream::empty()) as ModelEventStream)
            }
        })
    }
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        reasoning: true,
        tool_calls: true,
        streaming: true,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        system: SystemPromptSnapshot::new(vec!["stable system".to_owned()]),
        conversation: ConversationSnapshot::default(),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn all_reasons() -> BTreeSet<ModelRetryReason> {
    BTreeSet::from([
        ModelRetryReason::Connection,
        ModelRetryReason::Timeout,
        ModelRetryReason::RateLimited,
        ModelRetryReason::Unavailable,
    ])
}

fn policy(delays: Vec<Duration>) -> ModelRetryPolicy {
    ModelRetryPolicy::new(all_reasons(), delays, Duration::from_secs(10))
}

fn connection_error(message: &str) -> ModelError {
    ModelError::Transport {
        kind: ModelTransportErrorKind::Connection,
        message: message.to_owned(),
    }
}

fn timeout_error() -> ModelError {
    ModelError::Transport {
        kind: ModelTransportErrorKind::Timeout,
        message: "timed out".to_owned(),
    }
}

fn interrupted_error() -> ModelError {
    ModelError::Transport {
        kind: ModelTransportErrorKind::Interrupted,
        message: "stream reset".to_owned(),
    }
}

fn rate_limited(retry_after_ms: Option<u64>) -> ModelError {
    ModelError::RateLimited {
        message: "slow down".to_owned(),
        retry_after_ms,
    }
}

fn unavailable(retry_after_ms: Option<u64>) -> ModelError {
    ModelError::Unavailable {
        message: "overloaded".to_owned(),
        status: Some(503),
        retry_after_ms,
    }
}

fn terminal(error: ModelError) -> ScriptAction {
    ScriptAction::Stream(vec![ModelEvent::TurnFailed { error }])
}

#[tokio::test]
async fn explicitly_allowed_transient_reasons_retry_until_stream_establishes() {
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(connection_error("offline")),
        ScriptAction::Fail(timeout_error()),
        ScriptAction::Fail(rate_limited(None)),
        ScriptAction::Fail(unavailable(None)),
        ScriptAction::Stream(Vec::new()),
    ]));
    let observer = Arc::new(AttemptCollector::default());
    let service = RetryingModelService::with_observer(
        inner.clone(),
        policy(vec![Duration::ZERO; 4]),
        observer.clone(),
    );

    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("fifth attempt should establish");
    drop(stream);
    assert_eq!(inner.calls().len(), 5);
    let events = observer.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelAttemptEvent::Started { .. }))
            .count(),
        5
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelAttemptEvent::RetryScheduled { .. }))
            .count(),
        4
    );
    assert!(matches!(
        events.last(),
        Some(ModelAttemptEvent::StreamEstablished { attempt: 5, .. })
    ));
}

#[tokio::test]
async fn non_retryable_errors_never_consume_a_second_action() {
    let errors = [
        ModelError::Config("bad config".to_owned()),
        ModelError::Auth("bad key".to_owned()),
        interrupted_error(),
        ModelError::Provider {
            message: "bad request".to_owned(),
            status: Some(400),
        },
        ModelError::ContextOverflow {
            message: "too large".to_owned(),
        },
        ModelError::Protocol("bad frame".to_owned()),
        ModelError::ToolArguments("bad JSON".to_owned()),
        ModelError::Cancelled,
    ];

    for expected in errors {
        let inner = Arc::new(ScriptedService::new([
            ScriptAction::Fail(expected.clone()),
            ScriptAction::Stream(Vec::new()),
        ]));
        let service = RetryingModelService::new(inner.clone(), policy(vec![Duration::ZERO]));
        let error = service
            .stream(request(), ModelCallContext::default())
            .await
            .err()
            .expect("non-retryable error should return");
        assert_eq!(error, expected);
        assert_eq!(inner.calls().len(), 1);
    }
}

#[tokio::test]
async fn empty_delays_and_finite_table_bound_attempt_count_exactly() {
    let first = connection_error("first");
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(first.clone()),
        ScriptAction::Stream(Vec::new()),
    ]));
    let service = RetryingModelService::new(inner.clone(), policy(Vec::new()));
    assert_eq!(
        service
            .stream(request(), ModelCallContext::default())
            .await
            .err(),
        Some(first)
    );
    assert_eq!(inner.calls().len(), 1);

    let third = connection_error("third");
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(connection_error("first")),
        ScriptAction::Fail(connection_error("second")),
        ScriptAction::Fail(third.clone()),
        ScriptAction::Stream(Vec::new()),
    ]));
    let service =
        RetryingModelService::new(inner.clone(), policy(vec![Duration::ZERO, Duration::ZERO]));
    assert_eq!(
        service
            .stream(request(), ModelCallContext::default())
            .await
            .err(),
        Some(third)
    );
    assert_eq!(inner.calls().len(), 3);
}

#[tokio::test]
async fn every_attempt_reuses_the_request_and_overwrites_trace_attempt_from_one() {
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(connection_error("first")),
        ScriptAction::Fail(connection_error("second")),
        ScriptAction::Stream(Vec::new()),
    ]));
    let service =
        RetryingModelService::new(inner.clone(), policy(vec![Duration::ZERO, Duration::ZERO]));
    let original = request();
    let trace = TraceContext::new("logical-call")
        .with_attempt(NonZeroU32::new(99).expect("fixture attempt should be non-zero"));

    let stream = service
        .stream(
            original.clone(),
            ModelCallContext::default().with_trace(trace),
        )
        .await
        .expect("third attempt should establish");
    drop(stream);

    let calls = inner.calls();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|call| call.request == original));
    assert_eq!(
        calls
            .iter()
            .map(|call| {
                call.trace
                    .as_ref()
                    .and_then(|trace| trace.attempt)
                    .map(NonZeroU32::get)
            })
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2), Some(3)]
    );
}

#[tokio::test(start_paused = true)]
async fn retry_after_merges_with_policy_delay_and_respects_cap() {
    for (policy_delay, retry_after_ms, expected_delay_ms) in [(100, 250, 250), (500, 250, 500)] {
        let inner = Arc::new(ScriptedService::new([
            ScriptAction::Fail(rate_limited(Some(retry_after_ms))),
            ScriptAction::Stream(Vec::new()),
        ]));
        let observer = Arc::new(AttemptCollector::default());
        let service = RetryingModelService::with_observer(
            inner,
            policy(vec![Duration::from_millis(policy_delay)]),
            observer.clone(),
        );
        let stream = service
            .stream(request(), ModelCallContext::default())
            .await
            .expect("retry should establish");
        drop(stream);
        assert!(observer.events().iter().any(|event| matches!(
            event,
            ModelAttemptEvent::RetryScheduled { delay_ms, .. }
                if *delay_ms == expected_delay_ms
        )));
    }

    let original = unavailable(Some(250));
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(original.clone()),
        ScriptAction::Stream(Vec::new()),
    ]));
    let observer = Arc::new(AttemptCollector::default());
    let service = RetryingModelService::with_observer(
        inner.clone(),
        ModelRetryPolicy::new(
            all_reasons(),
            vec![Duration::from_millis(100)],
            Duration::from_millis(200),
        ),
        observer.clone(),
    );
    assert_eq!(
        service
            .stream(request(), ModelCallContext::default())
            .await
            .err(),
        Some(original)
    );
    assert_eq!(inner.calls().len(), 1);
    assert!(matches!(
        observer.events().last(),
        Some(ModelAttemptEvent::EstablishmentFailed {
            will_retry: false,
            ..
        })
    ));
}

#[tokio::test]
async fn cancellation_before_first_attempt_emits_nothing_and_calls_nothing() {
    let inner = Arc::new(ScriptedService::new([ScriptAction::Stream(Vec::new())]));
    let observer = Arc::new(AttemptCollector::default());
    let service = RetryingModelService::with_observer(
        inner.clone(),
        policy(vec![Duration::ZERO]),
        observer.clone(),
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        service
            .stream(request(), ModelCallContext::new(cancellation))
            .await,
        Err(ModelError::Cancelled)
    ));
    assert!(inner.calls().is_empty());
    assert!(observer.events().is_empty());
}

#[tokio::test]
async fn cancellation_before_retry_wait_prevents_the_next_attempt() {
    let cancellation = CancellationToken::new();
    let observer = Arc::new(CancellingObserver {
        cancellation: cancellation.clone(),
        events: Mutex::new(Vec::new()),
    });
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(connection_error("offline")),
        ScriptAction::Stream(Vec::new()),
    ]));
    let service =
        RetryingModelService::with_observer(inner.clone(), policy(vec![Duration::ZERO]), observer);

    assert!(matches!(
        service
            .stream(request(), ModelCallContext::new(cancellation))
            .await,
        Err(ModelError::Cancelled)
    ));
    assert_eq!(inner.calls().len(), 1);
}

#[tokio::test]
async fn cancellation_during_retry_wait_returns_without_sleeping_or_calling_again() {
    let cancellation = CancellationToken::new();
    let observer = Arc::new(AttemptCollector::default());
    let inner = Arc::new(ScriptedService::new([
        ScriptAction::Fail(connection_error("offline")),
        ScriptAction::Stream(Vec::new()),
    ]));
    let service = Arc::new(RetryingModelService::with_observer(
        inner.clone(),
        policy(vec![Duration::from_secs(3_600)]),
        observer.clone(),
    ));
    let task = {
        let service = service.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            service
                .stream(request(), ModelCallContext::new(cancellation))
                .await
                .map(|_| ())
        })
    };
    observer.retry_scheduled.notified().await;
    cancellation.cancel();

    assert!(matches!(
        task.await.expect("retry task should join"),
        Err(ModelError::Cancelled)
    ));
    assert_eq!(inner.calls().len(), 1);
}

#[tokio::test]
async fn cancellation_while_attempt_is_establishing_wins_over_late_success() {
    let entered = Arc::new(Notify::new());
    let inner = Arc::new(BlockingService {
        capabilities: capabilities(),
        entered: entered.clone(),
        calls: AtomicUsize::new(0),
    });
    let observer = Arc::new(AttemptCollector::default());
    let service = Arc::new(RetryingModelService::with_observer(
        inner.clone(),
        policy(vec![Duration::ZERO]),
        observer.clone(),
    ));
    let cancellation = CancellationToken::new();
    let task = {
        let service = service.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            service
                .stream(request(), ModelCallContext::new(cancellation))
                .await
                .map(|_| ())
        })
    };
    entered.notified().await;
    cancellation.cancel();

    assert!(matches!(
        task.await.expect("attempt task should join"),
        Err(ModelError::Cancelled)
    ));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        observer.events().last(),
        Some(ModelAttemptEvent::EstablishmentFailed {
            error: ModelError::Cancelled,
            will_retry: false,
            ..
        })
    ));
}

#[tokio::test]
async fn established_stream_failures_are_returned_verbatim_without_retry() {
    let errors = [
        interrupted_error(),
        ModelError::Protocol("bad frame".to_owned()),
        ModelError::Cancelled,
        rate_limited(Some(0)),
    ];

    for expected in errors {
        let inner = Arc::new(ScriptedService::new([
            terminal(expected.clone()),
            ScriptAction::Fail(connection_error("must not be reached")),
        ]));
        let service = RetryingModelService::new(inner.clone(), policy(vec![Duration::ZERO]));
        let stream = service
            .stream(request(), ModelCallContext::default())
            .await
            .expect("event stream itself should establish");
        let events: Vec<_> = stream.collect().await;
        assert_eq!(
            events,
            vec![ModelEvent::TurnFailed {
                error: expected.clone()
            }]
        );
        assert_eq!(inner.calls().len(), 1);
    }
}

#[tokio::test]
async fn missing_or_dropping_observer_does_not_change_retry_result() {
    for observer in [false, true] {
        let inner = Arc::new(ScriptedService::new([
            ScriptAction::Fail(connection_error("offline")),
            ScriptAction::Stream(Vec::new()),
        ]));
        let service = if observer {
            RetryingModelService::with_observer(
                inner.clone(),
                policy(vec![Duration::ZERO]),
                Arc::new(DroppingObserver),
            )
        } else {
            RetryingModelService::new(inner.clone(), policy(vec![Duration::ZERO]))
        };
        let stream = service
            .stream(request(), ModelCallContext::default())
            .await
            .expect("retry should establish");
        drop(stream);
        assert_eq!(inner.calls().len(), 2);
    }
}

#[test]
fn capabilities_context_window_and_attempt_events_are_transparent_and_serializable() {
    let inner = Arc::new(ScriptedService::new([]));
    let service = RetryingModelService::new(inner, policy(Vec::new()));
    assert_eq!(service.capabilities(), &capabilities());
    assert_eq!(service.context_window_tokens(), 128_000);

    let event = ModelAttemptEvent::EstablishmentFailed {
        trace: Some(
            TraceContext::new("call-1")
                .with_attempt(NonZeroU32::new(1).expect("attempt should be non-zero")),
        ),
        attempt: 1,
        error: timeout_error(),
        retry_reason: Some(ModelRetryReason::Timeout),
        will_retry: true,
    };
    let json = serde_json::to_vec(&event).expect("attempt event should serialize");
    assert_eq!(
        serde_json::from_slice::<ModelAttemptEvent>(&json)
            .expect("attempt event should deserialize"),
        event
    );
}

#[tokio::test]
async fn concurrent_logical_calls_keep_attempts_and_failures_isolated() {
    let inner = Arc::new(PerTraceService::new());
    let observer = Arc::new(AttemptCollector::default());
    let service = RetryingModelService::with_observer(
        inner.clone(),
        policy(vec![Duration::ZERO]),
        observer.clone(),
    );
    let first_trace = TraceContext::new("first");
    let second_trace = TraceContext::new("second");

    let (first, second) = tokio::join!(
        service.stream(
            request(),
            ModelCallContext::default().with_trace(first_trace)
        ),
        service.stream(
            request(),
            ModelCallContext::default().with_trace(second_trace)
        )
    );
    drop(first.expect("first call should establish on retry"));
    drop(second.expect("second call should establish on retry"));

    let traces = inner.traces();
    for correlation_id in ["first", "second"] {
        assert_eq!(
            traces
                .iter()
                .filter(|trace| trace.correlation_id == correlation_id)
                .filter_map(|trace| trace.attempt.map(NonZeroU32::get))
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
    for event in observer.events() {
        match event {
            ModelAttemptEvent::Started { trace, attempt }
            | ModelAttemptEvent::StreamEstablished { trace, attempt }
            | ModelAttemptEvent::EstablishmentFailed { trace, attempt, .. } => {
                assert_eq!(
                    trace.and_then(|trace| trace.attempt).map(NonZeroU32::get),
                    Some(attempt)
                );
            }
            ModelAttemptEvent::RetryScheduled {
                trace,
                next_attempt,
                ..
            } => {
                assert_eq!(
                    trace.and_then(|trace| trace.attempt).map(NonZeroU32::get),
                    Some(next_attempt)
                );
            }
        }
    }
}
