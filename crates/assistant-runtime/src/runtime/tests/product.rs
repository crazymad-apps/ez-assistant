use super::*;

use assistant_protocol::{
    ConversationItem, ConversationOwner, GetApplicationSnapshotRequest,
    GetConversationPageAroundRunRequest, GetSessionViewRequest, GetToolDetailRequest,
    InterruptRunRequest, ListConversationPageRequest, MessageFeedback,
    PrioritizeQueuedInputRequest, QueueExecutionState, ReenterFromUserMessageRequest,
    SetMessageFeedbackRequest,
};

#[tokio::test]
async fn markdown_export_contains_product_content_without_runtime_metadata() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "export-answer",
            "exported answer",
        )))],
    )));
    let session = runtime
        .create_session(CreateSessionRequest {
            title: Some("Export title".to_owned()),
            ..CreateSessionRequest::default()
        })
        .await
        .expect("session");
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id.clone(),
            message: "exported question".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("submit");
    wait_for_terminal(&runtime, &session.session.session_id, &submitted.run.run_id).await;

    let markdown = runtime
        .export_session_markdown(&session.session.session_id)
        .await
        .expect("export");
    assert!(markdown.starts_with("# Export title\n\n"));
    assert!(markdown.contains("## 用户\n\nexported question"));
    assert!(markdown.contains("## 助手\n\nexported answer"));
    assert!(!markdown.contains("provider_state"));
    assert!(!markdown.contains("agent_readable_path"));
}

#[tokio::test]
async fn completed_assistant_turn_exposes_the_reliable_run_finish_time() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "finished-answer",
            "done",
        )))],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "finish time".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("input")
        .run;
    wait_for_terminal(&runtime, &session_id, &run.run_id).await;

    let message = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: ConversationOwner::MainSession { session_id },
            cursor: None,
            limit: 20,
        })
        .await
        .expect("conversation")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message),
            ConversationItem::User(_) => None,
        })
        .expect("assistant message");

    assert_eq!(
        message.status,
        Some(assistant_protocol::RunStatus::Completed)
    );
    assert!(message.finished_at_ms.is_some());
}

#[tokio::test]
async fn assistant_feedback_is_persisted_in_the_conversation_projection_and_can_be_cleared() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "feedback-answer",
            "answer",
        )))],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "question".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("input")
        .run;
    wait_for_terminal(&runtime, &session_id, &run.run_id).await;
    let owner = ConversationOwner::MainSession {
        session_id: session_id.clone(),
    };
    let message_id = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 20,
        })
        .await
        .expect("page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message.message_id),
            ConversationItem::User(_) => None,
        })
        .expect("assistant message");
    runtime
        .set_message_feedback(SetMessageFeedbackRequest {
            session_id: session_id.clone(),
            message_id: message_id.clone(),
            feedback: Some(MessageFeedback::Positive),
        })
        .await
        .expect("feedback");
    let feedback = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 20,
        })
        .await
        .expect("feedback page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) if message.message_id == message_id => {
                Some(message.feedback)
            }
            _ => None,
        });
    assert_eq!(feedback, Some(Some(MessageFeedback::Positive)));
    runtime
        .set_message_feedback(SetMessageFeedbackRequest {
            session_id,
            message_id,
            feedback: None,
        })
        .await
        .expect("clear feedback");
    let cleared = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner,
            cursor: None,
            limit: 20,
        })
        .await
        .expect("cleared page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message.feedback),
            ConversationItem::User(_) => None,
        });
    assert_eq!(cleared, Some(None));
}

#[tokio::test]
async fn product_event_envelopes_and_application_snapshot_share_a_waterline() {
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            Vec::<ModelScript>::new(),
        )),
        ToolSetSnapshot::default(),
    );
    let mut events = runtime.subscribe_event_envelopes();
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let event = events.recv().await.expect("session event");
    assert_eq!(event.sequence, 1);
    assert!(matches!(
        event.event,
        RuntimeEvent::SessionCreated { session }
            if session.session_id == created.session.session_id
    ));

    let snapshot = runtime
        .get_application_snapshot(GetApplicationSnapshotRequest::default())
        .await
        .expect("application snapshot")
        .snapshot;
    assert_eq!(snapshot.observed_sequence, 1);
    assert_eq!(snapshot.value.active_sessions.len(), 1);
    assert!(snapshot.value.capabilities.conversation_paging);
}

#[tokio::test]
async fn conversation_pages_are_latest_first_queries_with_generation_bound_cursors() {
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [
                ModelScript::Events(message_events(&assistant_text("a-page-1", "first"))),
                ModelScript::Events(message_events(&assistant_text("a-page-2", "second"))),
            ],
        )),
        ToolSetSnapshot::default(),
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    for message in ["one", "two"] {
        let submitted = runtime
            .submit_input(SubmitInputRequest {
                session_id: session.session.session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
                variant: assistant_protocol::AgentVariant::Build,
            })
            .await
            .expect("submit");
        wait_for_terminal(&runtime, &session.session.session_id, &submitted.run.run_id).await;
    }

    let owner = ConversationOwner::MainSession {
        session_id: session.session.session_id.clone(),
    };
    let latest = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 2,
        })
        .await
        .expect("latest page")
        .snapshot
        .value;
    assert!(latest.has_more);
    assert!(matches!(
        &latest.items[0],
        ConversationItem::User(message) if message.text == "two"
    ));
    assert!(matches!(
        &latest.items[1],
        ConversationItem::Assistant(message)
            if matches!(&message.segments[0], assistant_protocol::AssistantSegment::Text { text, .. } if text == "second")
    ));

    let old_cursor = latest.previous_cursor.clone().expect("older cursor");
    let old_generation = latest.generation;
    let older = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner,
            cursor: Some(old_cursor.clone()),
            limit: 2,
        })
        .await
        .expect("older page")
        .snapshot
        .value;
    assert!(!older.has_more);
    assert!(matches!(
        &older.items[0],
        ConversationItem::User(message) if message.text == "one"
    ));

    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session.session.session_id,
        })
        .await
        .expect("session view")
        .snapshot;
    assert_eq!(view.value.runs.len(), 2);
    assert!(view.value.queue.items.is_empty());
    assert!(view.observed_sequence >= 5);
    let around = runtime
        .get_conversation_page_around_run(GetConversationPageAroundRunRequest {
            session_id: view.value.session.session_id.clone(),
            run_id: view.value.runs[1].run_id.clone(),
            limit: 2,
        })
        .await
        .expect("page around run");
    assert!(around.snapshot.value.items.iter().any(|item| {
        matches!(item, ConversationItem::Assistant(message) if message.message_id == around.anchor_message_id)
    }));

    let ConversationItem::User(first_user) = &older.items[0] else {
        panic!("older page starts with user")
    };
    let rewritten = runtime
        .reenter_from_user_message(ReenterFromUserMessageRequest {
            session_id: view.value.session.session_id.clone(),
            message_id: first_user.message_id.clone(),
            message: "replacement".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("rewrite conversation");
    wait_for_terminal(
        &runtime,
        &view.value.session.session_id,
        &rewritten.run.run_id,
    )
    .await;
    let latest_after_rewrite = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: ConversationOwner::MainSession {
                session_id: view.value.session.session_id.clone(),
            },
            cursor: None,
            limit: 2,
        })
        .await
        .expect("latest page after rewrite")
        .snapshot
        .value;
    assert_eq!(latest_after_rewrite.generation, old_generation + 1);
    assert!(matches!(
        runtime
            .list_conversation_page(ListConversationPageRequest {
                owner: ConversationOwner::MainSession {
                    session_id: view.value.session.session_id,
                },
                cursor: Some(old_cursor),
                limit: 2,
            })
            .await,
        Err(RuntimeError::SnapshotStale)
    ));
}

#[tokio::test]
async fn queue_priority_uses_revision_and_user_interrupt_pauses_remaining_inputs() {
    let entered = Arc::new(Notify::new());
    let runtime = runtime_with_tools(
        Arc::new(CancellationAwareModel {
            capabilities: model_capabilities(false),
            entered: entered.clone(),
        }),
        ToolSetSnapshot::default(),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let active = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "active".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("active input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model entered");
    let second = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "second".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("second input");
    let third = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "third".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("third input");
    let before = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value
        .queue;
    assert_eq!(
        before
            .items
            .iter()
            .map(|item| &item.input_id)
            .collect::<Vec<_>>(),
        vec![&second.input_id, &third.input_id]
    );

    let prioritized = runtime
        .prioritize_queued_input(PrioritizeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: third.input_id.clone(),
            expected_revision: before.revision,
        })
        .await
        .expect("prioritize input")
        .queue;
    assert_eq!(prioritized.items[0].input_id, third.input_id);
    assert!(matches!(
        runtime
            .prioritize_queued_input(PrioritizeQueuedInputRequest {
                session_id: session_id.clone(),
                input_id: second.input_id,
                expected_revision: before.revision,
            })
            .await,
        Err(RuntimeError::QueueConflict)
    ));

    let interrupted = runtime
        .interrupt_run(InterruptRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("interrupt active run");
    assert_eq!(interrupted.queue.state, QueueExecutionState::PausedByUser);
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &active.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    let paused = runtime
        .get_session_view(GetSessionViewRequest { session_id })
        .await
        .expect("paused session view")
        .snapshot
        .value
        .queue;
    assert_eq!(paused.state, QueueExecutionState::PausedByUser);
    assert_eq!(paused.items[0].input_id, third.input_id);
}

#[tokio::test]
async fn tool_detail_is_loaded_by_stable_owner_message_and_call_ids() {
    let tool = ScriptedTool::succeed("detail_tool", json!({"saved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("register tool");
    let tool_message_id = MessageId::new("assistant-detail-tool").expect("message id");
    let call_id = ToolCallId::new("detail-call").expect("call id");
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8_192,
            [
                ModelScript::Events(message_events(&AssistantMessage {
                    id: tool_message_id.clone(),
                    model: ModelIdentity::new(
                        ProviderId::new("fixture").expect("provider id"),
                        "fixture-model",
                    ),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("detail_tool").expect("tool name"),
                        arguments: json!({"path": "report.txt"}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                })),
                ModelScript::Events(message_events(&assistant_text(
                    "assistant-detail-final",
                    "saved",
                ))),
            ],
        )),
        registry.snapshot(),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    set_auto_approval(&runtime, &session_id).await;
    let run = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "save a report".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("submit");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let page = runtime
        .get_conversation_page_around_run(GetConversationPageAroundRunRequest {
            session_id: session_id.clone(),
            run_id: run.run.run_id.clone(),
            limit: 8,
        })
        .await
        .expect("conversation page")
        .snapshot
        .value;
    let tool_event = page.items.iter().find_map(|item| {
        let ConversationItem::Assistant(message) = item else {
            return None;
        };
        message.segments.iter().find_map(|segment| {
            let assistant_protocol::AssistantSegment::ToolGroup { tools } = segment else {
                return None;
            };
            tools.first()
        })
    });
    assert!(matches!(
        tool_event.map(|event| &event.input),
        Some(assistant_protocol::ToolInputSnapshot::File { path, .. }) if path == "report.txt"
    ));

    let session_view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    assert_eq!(session_view.file_references.len(), 1);
    assert_eq!(
        session_view.file_references[0].file.display_name,
        "report.txt"
    );
    assert_eq!(
        session_view.file_references[0].message_id.as_str(),
        tool_message_id.as_str()
    );
    assert_eq!(
        session_view.file_references[0].call_id.as_str(),
        call_id.as_str()
    );

    let detail = runtime
        .get_tool_detail(GetToolDetailRequest {
            owner: ConversationOwner::MainSession { session_id },
            message_id: assistant_protocol::MessageId::new(tool_message_id.as_str())
                .expect("protocol message id"),
            call_id: assistant_protocol::ToolCallId::new(call_id.as_str())
                .expect("protocol call id"),
        })
        .await
        .expect("tool detail")
        .snapshot
        .value;
    assert_eq!(detail.tool_name, "detail_tool");
    assert_eq!(
        detail.status,
        assistant_protocol::ToolActivityStatus::Completed
    );
    assert!(matches!(
        detail.input,
        assistant_protocol::ToolInputSnapshot::File { path, .. } if path == "report.txt"
    ));
    assert_eq!(detail.result_summary.as_deref(), Some("{\"saved\":true}"));
    assert_eq!(
        detail.request_json.as_deref(),
        Some("{\n  \"path\": \"report.txt\"\n}")
    );
    assert_eq!(
        detail.result_json.as_deref(),
        Some("{\n  \"saved\": true\n}")
    );
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].display_path.as_deref(), Some("report.txt"));
    let resolved = runtime
        .resolve_tool_file_resource(
            &detail.owner,
            &detail.message_id,
            &detail.files[0].resource_ref_id,
        )
        .await
        .expect("stable tool resource");
    assert!(resolved.path.ends_with("report.txt"));
    assert_eq!(resolved.display_name, "report.txt");
}
