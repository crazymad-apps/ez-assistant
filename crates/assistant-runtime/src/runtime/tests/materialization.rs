use super::*;

fn manifest(message: &str) -> assistant_protocol::SessionMaterializationManifest {
    assistant_protocol::SessionMaterializationManifest {
        idempotency_key: assistant_protocol::IdempotencyKey::new("materialization-runtime-key")
            .expect("key"),
        workspace_id: None,
        model_key: None,
        reasoning_effort: None,
        variant: assistant_protocol::AgentVariant::Build,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: message.to_owned(),
        mode: assistant_protocol::SubmitInputMode::StartGoal,
        attachments: vec![
            assistant_protocol::SessionMaterializationAttachment {
                selection_key: "selection-one".to_owned(),
                original_name: "one.txt".to_owned(),
                size_bytes: 3,
            },
            assistant_protocol::SessionMaterializationAttachment {
                selection_key: "selection-two".to_owned(),
                original_name: "two.txt".to_owned(),
                size_bytes: 3,
            },
        ],
        quotes: vec![assistant_protocol::QuotedTextSnapshot {
            quote_id: assistant_protocol::PartId::new("quote-materialized").expect("quote id"),
            exact: "selected text".to_owned(),
            prefix: "before".to_owned(),
            suffix: "after".to_owned(),
            source_owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: assistant_protocol::SessionId::new("source-session")
                    .expect("source session id"),
            },
            source_generation: 1,
            source_message_id: assistant_protocol::MessageId::new("source-message")
                .expect("source message id"),
            text_start_utf16: 0,
            text_end_utf16: 13,
            source_role: assistant_protocol::QuotedTextSourceRoleSnapshot::Assistant,
            source_label: "source session".to_owned(),
            source_created_at_ms: Some(123),
            source_available: true,
        }],
        skill_name: Some("review".to_owned()),
    }
}

fn staged() -> Vec<StagedSessionAttachment> {
    vec![
        StagedSessionAttachment {
            selection_key: "selection-one".to_owned(),
            original_name: "one.txt".to_owned(),
            staging_path: "/volatile/one.part".to_owned(),
            blob_hash: "1".repeat(64),
            size_bytes: 3,
            media_type: Some("text/plain".to_owned()),
        },
        StagedSessionAttachment {
            selection_key: "selection-two".to_owned(),
            original_name: "two.txt".to_owned(),
            staging_path: "/volatile/two.part".to_owned(),
            blob_hash: "2".repeat(64),
            size_bytes: 3,
            media_type: Some("text/plain".to_owned()),
        },
    ]
}

#[tokio::test]
async fn text_only_materialization_creates_no_provisional_attachment_or_session() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "materialized-answer",
            "done",
        )))],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    assert!(
        runtime
            .materialize_session(manifest("invalid file batch"), Vec::new())
            .await
            .is_err()
    );
    assert!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .expect("invalid materialization leaves no session")
            .sessions
            .is_empty()
    );
    let mut request = manifest("text only");
    request.idempotency_key =
        assistant_protocol::IdempotencyKey::new("text-only-materialization").expect("key");
    request.mode = assistant_protocol::SubmitInputMode::Normal;
    request.attachments.clear();
    request.quotes.clear();
    request.skill_name = None;

    let result = runtime
        .materialize_session(request, Vec::new())
        .await
        .expect("materialize text-only session");
    assert!(result.attachments.is_empty());
    assert_eq!(result.session.title, "text only");
    assert_eq!(
        wait_for_terminal(&runtime, &result.session.session_id, &result.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .expect("list text-only session")
            .sessions
            .len(),
        1
    );
}

#[tokio::test]
async fn first_send_materializes_goal_skill_quotes_and_files_once() {
    let entered = Arc::new(Notify::new());
    let model = Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
    });
    let mut runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    runtime.skill_package_source = Arc::new(super::sessions::StaticSkillPackageSource);

    let first = runtime
        .materialize_session(manifest("ship this change"), staged())
        .await
        .expect("materialize first send");
    assert_eq!(first.session.title, "ship this change");
    assert_eq!(first.attachments.len(), 2);
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .expect("list sessions")
            .sessions
            .len(),
        1
    );

    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("materialized run starts");
    let conversation = runtime
        .conversation_snapshot(&first.session.session_id)
        .await
        .expect("materialized conversation");
    let user = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user),
            _ => None,
        })
        .expect("materialized user message");
    assert_eq!(
        user.parts
            .iter()
            .filter(|part| matches!(part, UserPart::QuotedText(_)))
            .count(),
        1
    );
    assert!(
        user.parts
            .iter()
            .any(|part| { matches!(part, UserPart::QuotedText(quote) if !quote.source_available) })
    );
    assert_eq!(
        user.parts
            .iter()
            .filter_map(|part| match part {
                UserPart::FileReferences(files) => Some(files.files.len()),
                _ => None,
            })
            .sum::<usize>(),
        2
    );
    assert!(user.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(context) if context.kind.starts_with("goal_"))
    }));
    assert!(user.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(context) if context.kind == "skill_activation")
    }));

    let retry = runtime
        .materialize_session(manifest("ship this change"), staged())
        .await
        .expect("response-loss retry");
    assert_eq!(retry.session.session_id, first.session.session_id);
    assert_eq!(retry.input_id, first.input_id);
    assert_eq!(retry.run.run_id, first.run.run_id);
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .expect("list sessions after retry")
            .sessions
            .len(),
        1
    );

    let conflict = runtime
        .materialize_session(manifest("different content"), staged())
        .await
        .expect_err("same key with different manifest must conflict");
    assert_eq!(
        conflict.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::Conflict
    );
    let (goal_id, generation) = {
        let session = runtime
            .session(&first.session.session_id)
            .expect("materialized session");
        let state = session.lock_state().expect("session state");
        let goal = state.goal.as_ref().expect("materialized Goal");
        (goal.id.clone(), goal.generation)
    };
    runtime
        .stop_goal(assistant_protocol::StopGoalRequest {
            session_id: first.session.session_id,
            goal_id,
            expected_generation: generation,
        })
        .await
        .expect("stop materialized Goal");
}
