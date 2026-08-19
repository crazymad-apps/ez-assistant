use super::*;

use crate::StagedAttachmentUpload;
use assistant_protocol::{
    AgentVariant, ApprovalMode, ArchiveSessionRequest, DeleteSessionRequest, ForkSessionRequest,
    IdempotencyKey, ListAttachmentsRequest, ListConversationPageRequest, ListRunsRequest,
    PrepareDeleteSessionRequest, ReenterFromUserMessageRequest, RenameSessionRequest,
    RestoreSessionRequest, SessionLifecycle, SessionListFilter, SessionTitleOrigin,
    SetSessionApprovalModeRequest, SetSessionModelRequest, SetSessionPinnedRequest,
    SetSessionVariantRequest,
};

#[tokio::test]
async fn fork_creates_an_independent_session_at_a_reliable_assistant_message() {
    let assistant = assistant_text("assistant-fork-point", "fork here");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant))],
    ));
    let runtime = runtime(model);
    let source = runtime
        .create_session(CreateSessionRequest {
            title: Some("Source".to_owned()),
            model_key: None,
            workspace_id: None,
        })
        .await
        .expect("source session")
        .session;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            session_id: source.session_id.clone(),
            message: "first turn".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("submit source input");
    wait_for_terminal(&runtime, &source.session_id, &submitted.run.run_id).await;
    let page = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: source.session_id.clone(),
            },
            cursor: None,
            limit: 30,
        })
        .await
        .expect("source page")
        .snapshot
        .value;

    let forked = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-fork-point")
                .expect("fork point"),
            expected_generation: page.generation,
        })
        .await
        .expect("fork session")
        .session;

    assert_ne!(forked.session_id, source.session_id);
    assert_eq!(forked.title, "Source（分支）");
    assert_eq!(forked.model_key, source.model_key);
    assert_eq!(forked.message_count, 2);
    let forked_conversation = runtime
        .conversation_snapshot(&forked.session_id)
        .await
        .expect("forked conversation");
    assert_eq!(forked_conversation.messages.len(), 2);
    assert_eq!(
        runtime
            .conversation_snapshot(&source.session_id)
            .await
            .expect("source conversation")
            .messages
            .len(),
        2
    );
}

#[tokio::test]
async fn delete_requires_a_bound_one_time_confirmation_and_removes_only_the_target_session() {
    let runtime = runtime(empty_model());
    let target = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("target")
        .session;
    let other = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("other")
        .session;
    let mismatched = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: target.session_id.clone(),
        })
        .await
        .expect("prepare mismatched deletion");
    assert!(matches!(
        runtime
            .delete_session(DeleteSessionRequest {
                session_id: other.session_id.clone(),
                confirmation_token: mismatched.confirmation_token.clone(),
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    assert!(matches!(
        runtime
            .delete_session(DeleteSessionRequest {
                session_id: target.session_id.clone(),
                confirmation_token: mismatched.confirmation_token,
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));

    let prepared = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: target.session_id.clone(),
        })
        .await
        .expect("prepare deletion");
    assert_eq!(prepared.impact.message_count, 0);
    runtime
        .delete_session(DeleteSessionRequest {
            session_id: target.session_id.clone(),
            confirmation_token: prepared.confirmation_token,
        })
        .await
        .expect("delete target");

    assert!(matches!(
        runtime.get_session(assistant_protocol::GetSessionRequest {
            session_id: target.session_id,
        }),
        Err(RuntimeError::SessionNotFound { .. })
    ));
    assert!(
        runtime
            .get_session(assistant_protocol::GetSessionRequest {
                session_id: other.session_id,
            })
            .is_ok()
    );
}

#[tokio::test]
async fn faulted_idle_session_can_fork_reliable_history_and_be_deleted() {
    let assistant = assistant_text("assistant-faulted-fork", "reliable prefix");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant))],
    ));
    let runtime = runtime(model);
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("source session")
        .session;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            session_id: source.session_id.clone(),
            message: "persist a reliable turn".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("submit input");
    wait_for_terminal(&runtime, &source.session_id, &submitted.run.run_id).await;
    runtime
        .session(&source.session_id)
        .expect("source controller")
        .mark_faulted()
        .expect("fault source");
    let generation = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: source.session_id.clone(),
            },
            cursor: None,
            limit: 30,
        })
        .await
        .expect("reliable source conversation")
        .snapshot
        .value
        .generation;

    let forked = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-faulted-fork")
                .expect("fork point"),
            expected_generation: generation,
        })
        .await
        .expect("fork faulted source");
    assert_eq!(forked.session.message_count, 2);

    let prepared = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: source.session_id.clone(),
        })
        .await
        .expect("prepare faulted source deletion");
    runtime
        .delete_session(DeleteSessionRequest {
            session_id: source.session_id.clone(),
            confirmation_token: prepared.confirmation_token,
        })
        .await
        .expect("delete faulted source");
    assert!(matches!(
        runtime.get_session(assistant_protocol::GetSessionRequest {
            session_id: source.session_id,
        }),
        Err(RuntimeError::SessionNotFound { .. })
    ));
}

#[tokio::test]
async fn first_input_generates_a_bounded_title_without_overwriting_a_user_title() {
    let runtime = runtime(empty_model());
    let generated = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("generated session")
        .session;
    runtime
        .submit_input(SubmitInputRequest {
            session_id: generated.session_id.clone(),
            message: "\n  首行作为会话标题  \n后续正文".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("first input");
    let generated_summary = runtime
        .get_session(assistant_protocol::GetSessionRequest {
            session_id: generated.session_id,
        })
        .expect("generated summary")
        .session;
    assert_eq!(generated_summary.title, "首行作为会话标题");
    assert_eq!(
        generated_summary.title_origin,
        SessionTitleOrigin::Generated
    );

    let renamed = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("renamed session")
        .session;
    runtime
        .rename_session(RenameSessionRequest {
            session_id: renamed.session_id.clone(),
            title: "  用户标题  ".to_owned(),
        })
        .await
        .expect("rename");
    runtime
        .submit_input(SubmitInputRequest {
            session_id: renamed.session_id.clone(),
            message: "不应覆盖标题".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("first renamed input");
    let renamed_summary = runtime
        .get_session(assistant_protocol::GetSessionRequest {
            session_id: renamed.session_id,
        })
        .expect("renamed summary")
        .session;
    assert_eq!(renamed_summary.title, "用户标题");
    assert_eq!(renamed_summary.title_origin, SessionTitleOrigin::User);
}

#[tokio::test]
async fn pinned_sessions_sort_first_and_pin_is_idempotent() {
    let runtime = runtime(empty_model());
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first")
        .session;
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second")
        .session;
    let activity_time = first.updated_at_ms;
    for _ in 0..2 {
        runtime
            .set_session_pinned(SetSessionPinnedRequest {
                session_id: first.session_id.clone(),
                is_pinned: true,
            })
            .await
            .expect("pin");
    }
    let listed = runtime
        .list_sessions(ListSessionsRequest::default())
        .expect("sessions")
        .sessions;
    assert_eq!(listed[0].session_id, first.session_id);
    assert!(listed[0].is_pinned);
    assert_eq!(listed[0].updated_at_ms, activity_time);
    assert_eq!(listed[1].session_id, second.session_id);
}

#[tokio::test]
async fn title_changes_do_not_advance_session_activity_time() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    let activity_time = created.updated_at_ms;

    let renamed = runtime
        .rename_session(RenameSessionRequest {
            session_id: created.session_id,
            title: "Only metadata changed".to_owned(),
        })
        .await
        .expect("rename");

    assert_eq!(renamed.session.updated_at_ms, activity_time);
}

#[tokio::test]
async fn faulted_session_still_allows_non_execution_metadata_changes() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    runtime
        .session(&created.session_id)
        .expect("controller")
        .mark_faulted()
        .expect("fault session");

    let renamed = runtime
        .rename_session(RenameSessionRequest {
            session_id: created.session_id.clone(),
            title: "故障后仍可编辑".to_owned(),
        })
        .await
        .expect("rename faulted session");
    assert_eq!(renamed.session.title, "故障后仍可编辑");

    let pinned = runtime
        .set_session_pinned(SetSessionPinnedRequest {
            session_id: created.session_id,
            is_pinned: true,
        })
        .await
        .expect("pin faulted session");
    assert!(pinned.session.is_pinned);
}

#[tokio::test]
async fn archived_session_is_filtered_read_only_and_can_be_restored() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;

    let archived = runtime
        .archive_session(ArchiveSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("archive");
    assert_eq!(archived.session.lifecycle, SessionLifecycle::Archived);
    assert!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .expect("active sessions")
            .sessions
            .is_empty()
    );
    assert_eq!(
        runtime
            .list_sessions(ListSessionsRequest {
                filter: SessionListFilter::Archived,
            })
            .expect("archived sessions")
            .sessions
            .len(),
        1
    );
    assert!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("archived conversation")
            .messages
            .is_empty()
    );
    assert!(
        runtime
            .list_runs(ListRunsRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("archived runs")
            .runs
            .is_empty()
    );
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message: "not allowed".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::SessionArchived { .. })
    ));

    let restored = runtime
        .restore_session(RestoreSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("restore");
    assert_eq!(restored.session.lifecycle, SessionLifecycle::Active);
    assert_eq!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .expect("active sessions")
            .sessions[0]
            .session_id,
        session_id
    );
}

#[tokio::test]
async fn model_switch_changes_only_the_key_and_requires_an_idle_active_session() {
    let runtime = runtime(empty_model());
    runtime.config_registry.replace_document_for_test(
        &TEST_CONFIG.replace(
            "max_output_tokens = 4096",
            "max_output_tokens = 4096\n\n[models.alternate]\nprotocol = \"chat_completions\"\nprovider = \"fixture\"\nendpoint = \"https://api.example.test/v1\"\nmodel = \"alternate-model\"\napi_key = \"alternate-secret\"\ncontext_window_tokens = 8192\nmax_output_tokens = 4096",
        ),
    );
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let prompt = runtime
        .session_for_test(&session_id)
        .system_prompt()
        .clone();

    let changed = runtime
        .set_session_model(SetSessionModelRequest {
            session_id: session_id.clone(),
            model_key: assistant_protocol::ModelKey::new("alternate").expect("model key"),
        })
        .await
        .expect("change model");
    assert_eq!(changed.session.model_key.as_str(), "alternate");
    assert_eq!(
        runtime.session_for_test(&session_id).system_prompt(),
        &prompt
    );

    runtime
        .archive_session(ArchiveSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("archive");
    assert!(matches!(
        runtime
            .set_session_model(SetSessionModelRequest {
                session_id,
                model_key: assistant_protocol::ModelKey::new("fixture").expect("model key"),
            })
            .await,
        Err(RuntimeError::SessionArchived { .. })
    ));
}

#[tokio::test]
async fn active_run_blocks_archive_model_switch_and_history_reentry() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(1, None, entered.clone(), cleanup.clone());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let active = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "active input".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("active run");
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow the active tool once");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("tool entered");

    let variant = runtime
        .set_session_variant(SetSessionVariantRequest {
            session_id: session_id.clone(),
            variant: AgentVariant::Plan,
        })
        .await
        .expect("variant changes during an active run");
    assert_eq!(variant.session.current_variant, AgentVariant::Plan);
    let approval = runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session_id.clone(),
            approval_mode: ApprovalMode::Auto,
        })
        .await
        .expect("approval mode changes during an active run");
    assert_eq!(approval.session.approval_mode, ApprovalMode::Auto);
    let frozen = runtime
        .get_run(GetRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("active run snapshot")
        .run;
    assert_eq!(frozen.variant, AgentVariant::Build);
    assert_eq!(frozen.approval_mode, ApprovalMode::Ask);

    assert!(matches!(
        runtime
            .archive_session(ArchiveSessionRequest {
                session_id: session_id.clone(),
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));
    assert!(matches!(
        runtime
            .set_session_model(SetSessionModelRequest {
                session_id: session_id.clone(),
                model_key: assistant_protocol::ModelKey::new("fixture").expect("model key"),
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));
    assert!(matches!(
        runtime
            .reenter_from_user_message(ReenterFromUserMessageRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message_id: assistant_protocol::MessageId::new("unknown").expect("message id"),
                message: "replacement".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));

    runtime
        .cancel_run(CancelRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("cancel active");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("tool cleanup");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &active.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
}

#[tokio::test]
async fn reenter_from_user_destroys_the_target_and_tail_without_creating_a_branch() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("a-1", "first answer"))),
            ModelScript::Events(message_events(&assistant_text("a-2", "second answer"))),
            ModelScript::Events(message_events(&assistant_text("a-3", "replacement answer"))),
        ],
    )));
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let old_attachment = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: session_id.clone(),
            original_name: "old.txt".to_owned(),
            staging_path: "/volatile/old.part".to_owned(),
            blob_hash: "e".repeat(64),
            size_bytes: 10,
            media_type: None,
        })
        .await
        .expect("old attachment")
        .attachment;
    let replacement_attachment = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: session_id.clone(),
            original_name: "replacement.txt".to_owned(),
            staging_path: "/volatile/replacement.part".to_owned(),
            blob_hash: "f".repeat(64),
            size_bytes: 20,
            media_type: None,
        })
        .await
        .expect("replacement attachment")
        .attachment;
    let first = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "first question".to_owned(),
            attachment_ids: vec![old_attachment.attachment_id.clone()],
            idempotency_key: None,
        })
        .await
        .expect("first input");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let second = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "second question".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("second input");
    wait_for_terminal(&runtime, &session_id, &second.run.run_id).await;
    let before = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation before replacement");
    let target = before
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user.id.clone()),
            _ => None,
        })
        .expect("first user message");

    let replacement = runtime
        .reenter_from_user_message(ReenterFromUserMessageRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message_id: assistant_protocol::MessageId::new(target.as_str()).expect("message id"),
            message: "replacement question".to_owned(),
            attachment_ids: vec![replacement_attachment.attachment_id.clone()],
            idempotency_key: Some(IdempotencyKey::new("replace-1").expect("key")),
        })
        .await
        .expect("replace history");
    let repeated = runtime
        .reenter_from_user_message(ReenterFromUserMessageRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message_id: assistant_protocol::MessageId::new("already-removed").expect("message id"),
            message: "different retry payload is ignored".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: Some(IdempotencyKey::new("replace-1").expect("key")),
        })
        .await
        .expect("idempotent replacement retry");
    assert_eq!(repeated.input_id, replacement.input_id);
    assert_eq!(repeated.run.run_id, replacement.run.run_id);
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &replacement.run.run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let after = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation after replacement");
    let texts = after
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(user) => user.parts.iter().find_map(|part| match part {
                UserPart::Text(text) => Some(text.text.as_str()),
                UserPart::Injected(_) | UserPart::FileReferences(_) => None,
            }),
            ConversationMessage::Assistant(assistant) => {
                assistant.parts.iter().find_map(|part| match part {
                    AssistantPart::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(texts, ["replacement question", "replacement answer"]);
    let remaining_files = after
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => user.parts.iter().find_map(|part| match part {
                UserPart::FileReferences(files) => Some(&files.files),
                UserPart::Text(_) | UserPart::Injected(_) => None,
            }),
            _ => None,
        })
        .expect("replacement file references");
    assert_eq!(remaining_files.len(), 1);
    assert_eq!(
        remaining_files[0].readable_path,
        replacement_attachment.agent_readable_path
    );
    assert_eq!(
        runtime
            .list_attachments(ListAttachmentsRequest {
                session_id: session_id.clone(),
            })
            .expect("session attachments")
            .attachments
            .len(),
        2
    );
    let runs = runtime
        .list_runs(ListRunsRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("runs");
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(runs.runs[0].run_id, replacement.run.run_id);
    assert!(
        runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: first.run.run_id,
            })
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .get_session(GetSessionRequest { session_id })
            .expect("session")
            .session
            .message_count,
        2
    );
}
