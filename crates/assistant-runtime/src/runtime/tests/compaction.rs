use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent_model::{ModelEvent, ModelEventStream};
use assistant_protocol::GetConversationPageAroundRunRequest;

struct CallGatedModel {
    capabilities: ModelCapabilities,
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    calls: AtomicUsize,
    gate_call: usize,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ModelService for CallGatedModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let events = self
            .scripts
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("scripted call");
        let entered = self.entered.clone();
        let release = self.release.clone();
        let gate_call = self.gate_call;
        Box::pin(async move {
            if call == gate_call {
                entered.notify_one();
                release.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

async fn submit_completed_turn(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
    text: &str,
) -> RunId {
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: text.to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("turn accepted");
    let run_id = accepted.run.run_id.clone();
    assert_eq!(
        wait_for_terminal(runtime, session_id, &run_id).await.status,
        assistant_protocol::RunStatus::Completed
    );
    run_id
}

#[tokio::test]
async fn manual_compaction_replaces_history_without_creating_a_run_and_is_idempotent() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer-1", "first answer"))),
            ModelScript::Events(message_events(&assistant_text("answer-2", "second answer"))),
            ModelScript::Events(message_events(&assistant_text(
                "manual-summary",
                "first turn summarized",
            ))),
            ModelScript::Events(message_events(&assistant_text("answer-3", "third answer"))),
        ],
    ));
    let runtime = runtime(model.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = session.session.session_id;
    let first_run_id = submit_completed_turn(&runtime, &session_id, "first turn").await;
    submit_completed_turn(&runtime, &session_id, "second turn").await;
    let before = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    let operation_id =
        assistant_protocol::IdempotencyKey::new("compact-manual-1").expect("operation id");
    let mut events = runtime.subscribe_events();

    let result = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: operation_id.clone(),
            expected_generation: before.conversation_generation,
        })
        .await
        .expect("manual compact");
    let outcome = result.outcome.clone();
    assert!(matches!(
        outcome,
        assistant_protocol::CompactSessionOutcome::Compacted {
            source_generation: 1,
            result_generation: 2,
            compacted_message_count: 2,
            retained_message_count: 2,
        }
    ));
    assert!(result.session.active_compaction.is_none());
    let conversation = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("compacted conversation");
    assert!(matches!(
        conversation.messages.first(),
        Some(ConversationMessage::ContextSummary(summary))
            if summary.text == "first turn summarized"
    ));
    assert!(conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user) if user.parts.iter().any(|part| {
            matches!(part, UserPart::Text(text) if text.text == "second turn")
        }))
    }));
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("compacted view")
        .snapshot
        .value;
    assert_eq!(view.conversation_generation, 2);
    assert!(view.conversation.items.iter().any(|item| matches!(
        item,
        assistant_protocol::ConversationItem::ContextSummary { .. }
    )));
    assert!(view.conversation.items.iter().any(|item| matches!(
        item,
        assistant_protocol::ConversationItem::User(user) if user.text == "first turn"
    )));
    let first_run_page_result = runtime
        .get_conversation_page_around_run(GetConversationPageAroundRunRequest {
            session_id: session_id.clone(),
            run_id: first_run_id,
            limit: 10,
        })
        .await
        .expect("page around a run before the compaction boundary");
    let first_run_page = first_run_page_result.snapshot.value;
    assert!(first_run_page.items.iter().any(|item| matches!(
        item,
        assistant_protocol::ConversationItem::User(user) if user.text == "first turn"
    )));
    let exported = runtime
        .export_session_markdown(&session_id)
        .await
        .expect("export complete product history");
    assert!(exported.contains("first turn"));
    runtime
        .set_message_feedback(assistant_protocol::SetMessageFeedbackRequest {
            session_id: session_id.clone(),
            message_id: first_run_page_result.anchor_message_id,
            feedback: Some(assistant_protocol::MessageFeedback::Positive),
        })
        .await
        .expect("feedback on assistant before compaction boundary");
    assert_eq!(
        runtime
            .list_runs(assistant_protocol::ListRunsRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("runs")
            .runs
            .len(),
        2
    );

    let retry = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id,
            expected_generation: before.conversation_generation,
        })
        .await
        .expect("idempotent compact retry");
    assert_eq!(retry.outcome, outcome);
    submit_completed_turn(&runtime, &session_id, "third turn").await;
    let requests = model.take_requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].generation.max_output_tokens, Some(1_024));
    let next_conversation = &requests[3].conversation.messages;
    assert!(matches!(
        next_conversation.first(),
        Some(ConversationMessage::ContextSummary(summary))
            if summary.text == "first turn summarized"
    ));
    let user_texts = next_conversation
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(user) => Some(user.parts.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_texts, vec!["second turn", "third turn"]);

    let mut saw_started = false;
    let mut saw_finished = false;
    while let Ok(event) = events.try_recv() {
        match event {
            assistant_protocol::RuntimeEvent::SessionCompactionStarted {
                session_id: observed,
                compaction,
            } if observed == session_id => {
                saw_started = true;
                assert!(matches!(
                    compaction.trigger,
                    assistant_protocol::SessionCompactionTriggerSnapshot::Manual
                ));
            }
            assistant_protocol::RuntimeEvent::SessionCompactionFinished {
                session_id: observed,
                outcome: assistant_protocol::SessionCompactionFinishedOutcome::Compacted { .. },
                ..
            } if observed == session_id => saw_finished = true,
            _ => {}
        }
    }
    assert!(saw_started && saw_finished);
}

#[tokio::test]
async fn manual_compaction_noop_keeps_generation_and_clears_volatile_state() {
    let runtime = runtime(empty_model());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = session.session.session_id;
    let result = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("compact-noop")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect("no-op compact");
    assert_eq!(
        result.outcome,
        assistant_protocol::CompactSessionOutcome::NoOp
    );
    assert!(result.session.active_compaction.is_none());
    assert_eq!(
        runtime
            .get_session_view(GetSessionViewRequest { session_id })
            .await
            .expect("view")
            .snapshot
            .value
            .conversation_generation,
        1
    );
}

#[tokio::test]
async fn manual_compaction_rejects_a_session_with_an_active_run() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let model = Arc::new(CallGatedModel {
        capabilities: model_capabilities(false),
        scripts: Mutex::new(VecDeque::from([message_events(&assistant_text(
            "busy-answer",
            "answer",
        ))])),
        calls: AtomicUsize::new(0),
        gate_call: 0,
        entered: entered.clone(),
        release: release.clone(),
    });
    let runtime = runtime(model);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = session.session.session_id;
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "running turn".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("turn accepted");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model call starts");

    let error = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("compact-busy")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect_err("active run rejects compact");
    assert_eq!(
        error.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionNotIdle
    );
    assert!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session_id.clone(),
            })
            .expect("session")
            .session
            .active_compaction
            .is_none()
    );

    release.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
}

#[tokio::test]
async fn manual_compaction_failure_preserves_history_and_clears_volatile_state() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer-1", "first answer"))),
            ModelScript::Events(message_events(&assistant_text("answer-2", "second answer"))),
            ModelScript::FailEstablishment(ModelError::Config("summary unavailable".to_owned())),
        ],
    ));
    let runtime = runtime(model);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = session.session.session_id;
    submit_completed_turn(&runtime, &session_id, "first turn").await;
    submit_completed_turn(&runtime, &session_id, "second turn").await;
    let before = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("source conversation");
    let mut events = runtime.subscribe_events();

    let error = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("compact-failure")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect_err("summary failure rejects compact");
    assert!(matches!(
        &error,
        RuntimeError::ModelExecutionFailed {
            source: ModelError::Config(message),
        } if message == "summary unavailable"
    ));
    assert_eq!(
        error.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::ModelExecutionFailed
    );
    assert!(
        error
            .to_protocol_info()
            .message
            .contains("kind=configuration")
    );
    assert_eq!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("conversation remains available"),
        before
    );
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    assert_eq!(view.conversation_generation, 1);
    assert!(view.session.active_compaction.is_none());

    let mut saw_failed = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event,
            assistant_protocol::RuntimeEvent::SessionCompactionFinished {
                session_id: observed,
                outcome: assistant_protocol::SessionCompactionFinishedOutcome::Failed {
                    code: assistant_protocol::RuntimeErrorCode::ModelExecutionFailed,
                },
                ..
            } if observed == session_id
        ) {
            saw_failed = true;
        }
    }
    assert!(saw_failed);
}

#[tokio::test]
async fn manual_compaction_can_be_cancelled_and_rejects_new_input_while_active() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let model = Arc::new(CallGatedModel {
        capabilities: model_capabilities(false),
        scripts: Mutex::new(VecDeque::from([
            message_events(&assistant_text("cancel-answer-1", "first answer")),
            message_events(&assistant_text("cancel-answer-2", "second answer")),
            message_events(&assistant_text("cancel-summary", "summary")),
        ])),
        calls: AtomicUsize::new(0),
        gate_call: 2,
        entered: entered.clone(),
        release: release.clone(),
    });
    let runtime = Arc::new(runtime(model));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = session.session.session_id;
    submit_completed_turn(&runtime, &session_id, "first turn").await;
    submit_completed_turn(&runtime, &session_id, "second turn").await;
    let operation_id =
        assistant_protocol::IdempotencyKey::new("compact-cancel").expect("operation id");
    let compact_runtime = runtime.clone();
    let compact_session_id = session_id.clone();
    let compact_operation_id = operation_id.clone();
    let compact = tokio::spawn(async move {
        compact_runtime
            .compact_session(assistant_protocol::CompactSessionRequest {
                session_id: compact_session_id,
                operation_id: compact_operation_id,
                expected_generation: 1,
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("summary call starts");
    let active = runtime
        .get_session(GetSessionRequest {
            session_id: session_id.clone(),
        })
        .expect("session")
        .session
        .active_compaction
        .expect("active compaction");
    assert!(active.cancellable);
    let busy = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "must not queue during compact".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect_err("input is rejected while compacting");
    assert_eq!(
        busy.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionCompactionInProgress
    );
    runtime
        .cancel_session_compaction(assistant_protocol::CancelSessionCompactionRequest {
            session_id: session_id.clone(),
            operation_id: operation_id.clone(),
        })
        .await
        .expect("cancel accepted");
    compact.abort();
    assert!(
        compact
            .await
            .expect_err("request future was dropped")
            .is_cancelled()
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime
                .get_session(GetSessionRequest {
                    session_id: session_id.clone(),
                })
                .expect("session")
                .session
                .active_compaction
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime-owned compaction settles after request drop");
    let result = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: operation_id.clone(),
            expected_generation: 1,
        })
        .await
        .expect("cancelled result is idempotent");
    assert_eq!(
        result.outcome,
        assistant_protocol::CompactSessionOutcome::Cancelled
    );
    assert!(result.session.active_compaction.is_none());
    assert_eq!(
        runtime
            .get_session_view(GetSessionViewRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("view")
            .snapshot
            .value
            .conversation_generation,
        1
    );
    let missing = runtime
        .cancel_session_compaction(assistant_protocol::CancelSessionCompactionRequest {
            session_id,
            operation_id,
        })
        .await
        .expect_err("finished compact cannot be cancelled");
    assert_eq!(
        missing.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionCompactionNotFound
    );
}
