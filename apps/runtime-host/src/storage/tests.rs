use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use agent_core::ExchangeReceipt;
use agent_model::SystemPromptSnapshot;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FileReference,
    FileReferencesPart, FinishReason, MessageId, ModelIdentity, OpaqueProviderState, PartId,
    ProtocolId, ProviderId, ReasoningPart, TextPart, ToolCall, ToolCallId, ToolMessage, ToolName,
    ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use assistant_protocol::{
    AttachmentId, ChildTaskId, ChildTaskStatus, ConversationOwner, IdempotencyKey, InputId,
    MessageFeedback, ModelKey, PermissionDiagnosticCode, RunId, RunStatus, SessionId,
    SessionTitleOrigin, WorkspaceId,
};
use assistant_runtime::{
    ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementTarget, ConversationRewrite, ConversationWindowRequest,
    ForkedAttachmentReference, MessageFeedbackChange, ModelChange, NewAttachmentUpload,
    NewStoredChildTask, NewStoredInput, NewStoredSession, NewWorkspaceRegistration,
    PendingChildToolExchange, PendingToolExchange, PermissionDocument, PermissionEffect,
    PermissionFileOperation, PermissionFileRevision, PermissionFileScope, PermissionFileStore,
    QueuePriorityChange, RuntimeStore, SessionDeletion, SessionExecutionEnvironment, SessionFork,
    SessionPinnedChange, SessionTitleChange, StoreErrorKind, StoredAttachmentState,
    StoredChildTaskSettlement, StoredConversationState, StoredRunSettlement, StoredSession,
    StoredSessionLifecycle, StoredWorkspaceLifecycle, ToolExecutionStart, UserMessageCommit,
    VariantChange, WorkspaceRemoval,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::{
    DATA_DIRECTORY, DATABASE_FILE, LocalRuntimeStore, SESSIONS_DIRECTORY, StorageEngine,
    append_effect::AppendPurpose,
    body_path, child_body_path, child_task_directory, conversation,
    recovery::{AppendRequest, ReplacementPlan},
};

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("session id")
}

fn run_id(value: &str) -> RunId {
    RunId::new(value).expect("run id")
}

fn child_task_id(value: &str) -> ChildTaskId {
    ChildTaskId::new(value).expect("child task id")
}

fn workspace_id(value: &str) -> WorkspaceId {
    WorkspaceId::new(value).expect("workspace id")
}

fn new_session(value: &str, sessions_directory: &Path) -> NewStoredSession {
    let session_directory = sessions_directory.join(value);
    let private_directory = session_directory.join("private");
    NewStoredSession {
        session_id: session_id(value),
        title: format!("Session {value}"),
        title_origin: assistant_protocol::SessionTitleOrigin::Generated,
        model_key: ModelKey::new("fixture-model").expect("model key"),
        system_prompt: SystemPromptSnapshot::new(vec!["stable prompt".to_owned()]),
        environment: SessionExecutionEnvironment {
            workspace_id: None,
            working_directory: private_directory.to_string_lossy().into_owned(),
            workspace_private_directory: None,
            session_attachment_directory: session_directory
                .join("attachments")
                .to_string_lossy()
                .into_owned(),
            session_private_directory: private_directory.to_string_lossy().into_owned(),
        },
        current_variant: assistant_protocol::AgentVariant::Build,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        created_at_ms: 1_000,
    }
}

fn assert_default_session_permissions(session: &StoredSession) {
    let document = PermissionDocument::parse(
        &fs::read(
            Path::new(&session.environment.session_private_directory).join("permissions.json"),
        )
        .expect("default session permissions"),
    )
    .expect("valid default session permissions");
    assert_eq!(document.rules.len(), 11);
    assert!(
        document
            .rules
            .iter()
            .all(|rule| rule.effect == PermissionEffect::Allow)
    );
    assert_eq!(
        document
            .rules
            .iter()
            .filter(|rule| rule.variants == [assistant_protocol::AgentVariant::Build])
            .count(),
        3
    );
    assert_eq!(
        document
            .rules
            .iter()
            .filter(|rule| {
                matches!(
                    &rule.matcher,
                    assistant_runtime::PermissionMatcher::File(matcher)
                        if matcher.path == session.environment.session_private_directory
                            && matcher.path_match == assistant_runtime::PathMatch::Recursive
                )
            })
            .count(),
        7
    );
    assert_eq!(
        document
            .rules
            .iter()
            .filter(|rule| {
                matches!(
                    &rule.matcher,
                    assistant_runtime::PermissionMatcher::File(matcher)
                        if matcher.path == session.environment.session_attachment_directory
                            && matcher.path_match == assistant_runtime::PathMatch::Recursive
                            && matches!(
                                matcher.operation,
                                PermissionFileOperation::Read
                                    | PermissionFileOperation::List
                                    | PermissionFileOperation::Find
                                    | PermissionFileOperation::Search
                            )
                )
            })
            .count(),
        4
    );
}

fn user_message(value: &str, text: &str) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: MessageId::new(value).expect("message id"),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new(format!("part-{value}")).expect("part id"),
            text: text.to_owned(),
        })],
    })
}

fn assistant_message(value: &str, text: &str) -> ConversationMessage {
    ConversationMessage::Assistant(AssistantMessage {
        id: MessageId::new(value).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: PartId::new(format!("reasoning-{value}")).expect("part id"),
                text: "private reasoning".to_owned(),
            }),
            AssistantPart::Text(TextPart {
                id: PartId::new(format!("text-{value}")).expect("part id"),
                text: text.to_owned(),
            }),
        ],
        finish_reason: FinishReason::Stop,
        usage: None,
    })
}

fn tool_exchange() -> Vec<ConversationMessage> {
    let call_id = ToolCallId::new("call-1").expect("tool call id");
    vec![
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new("assistant-tool").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![
                AssistantPart::ProviderState(
                    OpaqueProviderState::new(
                        ProviderId::new("fixture").expect("provider id"),
                        ProtocolId::new("complete").expect("protocol id"),
                        "continuation",
                        "application/json",
                        1,
                        br#"{"cursor":"opaque"}"#.to_vec(),
                    )
                    .expect("provider state"),
                ),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new("echo_text").expect("tool name"),
                    arguments: serde_json::json!({"text": "hello"}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new("tool-result").expect("message id"),
            result: ToolResult {
                call_id,
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("hello".to_owned()),
            },
        }),
    ]
}

#[test]
fn session_navigation_metadata_and_feedback_survive_reopen() {
    let root = TempDir::new().expect("temp root");
    let mut engine = open_engine(&root);
    let session = session_id("s-navigation-persistence");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("input-navigation-persistence").expect("input id"),
            run_id: run_id("run-navigation-persistence"),
            session_id: session.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: raw_user_message("user-navigation-persistence", "first input"),
            generated_title: Some("Generated first input".to_owned()),
            accepted_at_ms: 1_500,
        })
        .expect("accept first input");
    let generated_title: String = engine
        .connection
        .query_row(
            "SELECT title FROM sessions WHERE session_id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .expect("generated title");
    assert_eq!(generated_title, "Generated first input");
    engine
        .rename_session(SessionTitleChange {
            session_id: session.clone(),
            title: "User title".to_owned(),
            changed_at_ms: 2_000,
        })
        .expect("rename");
    engine
        .set_session_pinned(SessionPinnedChange {
            session_id: session.clone(),
            is_pinned: true,
            changed_at_ms: 2_001,
        })
        .expect("pin");
    let message_id = assistant_protocol::MessageId::new("assistant-feedback").expect("message id");
    engine
        .set_message_feedback(MessageFeedbackChange {
            session_id: session.clone(),
            message_id: message_id.clone(),
            feedback: Some(MessageFeedback::Negative),
            changed_at_ms: 2_002,
        })
        .expect("feedback");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    let stored = recovered
        .sessions
        .iter()
        .find(|stored| stored.session_id == session)
        .expect("stored session");
    assert_eq!(stored.title, "User title");
    assert_eq!(stored.title_origin, SessionTitleOrigin::User);
    assert!(stored.is_pinned);
    assert_eq!(stored.updated_at_ms, 1_000);
    assert_eq!(
        reopened
            .load_message_feedback(&session)
            .expect("load feedback")[0]
            .feedback,
        MessageFeedback::Negative
    );
    reopened
        .set_message_feedback(MessageFeedbackChange {
            session_id: session.clone(),
            message_id,
            feedback: None,
            changed_at_ms: 2_003,
        })
        .expect("clear feedback");
    assert!(
        reopened
            .load_message_feedback(&session)
            .expect("cleared feedback")
            .is_empty()
    );
}

#[test]
fn session_activity_time_advances_only_when_a_run_settles() {
    let root = TempDir::new().expect("temp root");
    let mut engine = open_engine(&root);
    let session = session_id("s-activity-time");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    commit_completed_turn(&mut engine, &session, "activity-time", 2_000);
    engine
        .rename_session(SessionTitleChange {
            session_id: session.clone(),
            title: "Renamed after run".to_owned(),
            changed_at_ms: 3_000,
        })
        .expect("rename");
    engine
        .set_session_pinned(SessionPinnedChange {
            session_id: session.clone(),
            is_pinned: true,
            changed_at_ms: 3_001,
        })
        .expect("pin");

    let recovered = engine.load_runtime().expect("load runtime");
    let stored = recovered
        .sessions
        .iter()
        .find(|stored| stored.session_id == session)
        .expect("stored session");
    assert_eq!(stored.updated_at_ms, 2_002);
}

fn pending_tool_exchange(session: &str, run: &str, receipt: &str) -> PendingToolExchange {
    let ConversationMessage::Assistant(assistant) = tool_exchange().remove(0) else {
        unreachable!("fixture starts with assistant")
    };
    PendingToolExchange {
        receipt: ExchangeReceipt::new(receipt).expect("receipt"),
        session_id: session_id(session),
        run_id: run_id(run),
        assistant,
        created_at_ms: 2_000,
    }
}

fn pending_delegate_exchange(session: &str, run: &str, receipt: &str) -> PendingToolExchange {
    PendingToolExchange {
        receipt: ExchangeReceipt::new(receipt).expect("receipt"),
        session_id: session_id(session),
        run_id: run_id(run),
        assistant: AssistantMessage {
            id: MessageId::new(format!("assistant-{receipt}")).expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("call-1").expect("call id"),
                name: ToolName::new("delegate_task").expect("tool name"),
                arguments: serde_json::json!({"title": "recover child", "task": "inspect"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        },
        created_at_ms: 2_000,
    }
}

fn tool_results() -> Vec<ToolMessage> {
    tool_exchange()
        .into_iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(message) => Some(message),
            _ => None,
        })
        .collect()
}

fn start_tool(engine: &mut StorageEngine, session: &str, run: &str, receipt: &str) {
    engine
        .mark_tool_execution_started(ToolExecutionStart {
            receipt: ExchangeReceipt::new(receipt).expect("receipt"),
            session_id: session_id(session),
            run_id: run_id(run),
            call_id: assistant_protocol::ToolCallId::new("call-1").expect("call id"),
            started_at_ms: 2_200,
        })
        .expect("record tool started");
}

fn open_engine(root: &TempDir) -> StorageEngine {
    StorageEngine::open(root.path()).expect("open storage engine")
}

fn raw_user_message(value: &str, text: &str) -> UserMessage {
    let ConversationMessage::User(message) = user_message(value, text) else {
        unreachable!("fixture is a user message")
    };
    message
}

fn commit_completed_turn(
    engine: &mut StorageEngine,
    session: &SessionId,
    suffix: &str,
    accepted_at_ms: i64,
) {
    let input_id = InputId::new(format!("input-{suffix}")).expect("input id");
    let run_id = run_id(&format!("run-{suffix}"));
    let message = raw_user_message(&format!("user-{suffix}"), suffix);
    engine
        .accept_input(NewStoredInput {
            agent_variant: assistant_protocol::AgentVariant::Build,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            input_id: input_id.clone(),
            run_id: run_id.clone(),
            session_id: session.clone(),
            idempotency_key: None,
            message: message.clone(),
            generated_title: None,
            accepted_at_ms,
        })
        .expect("accept fixture input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: format!("commit-{suffix}"),
            input_id,
            run_id: run_id.clone(),
            session_id: session.clone(),
            message: Some(message),
            created_at_ms: accepted_at_ms + 1,
        })
        .expect("commit fixture user message");
    engine
        .settle_run(StoredRunSettlement {
            operation_id: format!("settle-{suffix}"),
            run_id,
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message(&format!("assistant-{suffix}"), suffix)],
            finished_at_ms: accepted_at_ms + 2,
        })
        .expect("settle fixture run");
}

#[test]
fn fork_and_delete_commit_consistently_across_sqlite_and_session_directories() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source_id = session_id("s-transfer-source");
    let forked_id = session_id("s-transfer-fork");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(source_id.as_str(), &sessions_directory))
        .expect("create source");
    commit_completed_turn(&mut engine, &source_id, "transfer", 2_000);
    let source_generation = engine
        .connection
        .query_row(
            "SELECT body_generation FROM sessions WHERE session_id = ?1",
            [source_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .expect("source generation") as u64;
    let source_conversation = engine
        .load_conversation(&source_id)
        .expect("source conversation");

    let forked = engine
        .fork_session(SessionFork {
            source_session_id: source_id.clone(),
            source_generation,
            session: new_session(forked_id.as_str(), &sessions_directory),
            conversation: source_conversation.clone(),
            attachments: Vec::new(),
        })
        .expect("fork session");
    assert_eq!(forked.session.session_id, forked_id);
    assert_eq!(forked.session.body_generation, 1);
    assert_default_session_permissions(&forked.session);
    assert_eq!(forked.conversation, source_conversation);
    assert_eq!(
        engine
            .load_conversation(&forked.session.session_id)
            .expect("forked conversation"),
        source_conversation
    );

    let impact = engine
        .inspect_session_deletion(&source_id)
        .expect("delete impact");
    assert_eq!(impact.message_count, 2);
    assert_eq!(impact.run_count, 1);
    engine
        .delete_session(SessionDeletion {
            session_id: source_id.clone(),
            operation_id: "delete-transfer-source".to_owned(),
            expected_impact: impact,
        })
        .expect("delete source");
    assert!(!engine.sessions_directory.join(source_id.as_str()).exists());
    assert!(engine.sessions_directory.join(forked_id.as_str()).exists());
    assert!(engine.inspect_session_deletion(&source_id).is_err());

    drop(engine);
    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.sessions.len(), 1);
    assert_eq!(recovered.sessions[0].session_id, forked_id);
}

#[test]
fn fork_clones_attachment_references_without_coupling_source_lifecycle() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source_id = session_id("s-transfer-attachment-source");
    let forked_id = session_id("s-transfer-attachment-fork");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(source_id.as_str(), &sessions_directory))
        .expect("create source");

    let bytes = b"fork attachment payload";
    let original_name = "fork-source.txt";
    let blob_hash = crate::attachment_hash::digest_bytes(original_name, bytes);
    let staging = engine.upload_staging_directory.join("fork-source.part");
    fs::write(&staging, bytes).expect("write staging");
    let source_attachment = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-transfer-source").expect("attachment id"),
            session_id: source_id.clone(),
            original_name: original_name.to_owned(),
            staging_path: staging.to_string_lossy().into_owned(),
            blob_hash: blob_hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_000,
        })
        .expect("upload source attachment");
    let source_readable_path = source_attachment.agent_readable_path.clone();
    let conversation = ConversationSnapshot::new(vec![
        ConversationMessage::User(UserMessage {
            id: MessageId::new("message-transfer-file").expect("message id"),
            parts: vec![UserPart::FileReferences(FileReferencesPart {
                id: PartId::new("part-transfer-file").expect("part id"),
                files: vec![FileReference {
                    original_name: original_name.to_owned(),
                    readable_path: source_readable_path.clone(),
                }],
            })],
        }),
        assistant_message("assistant-transfer-file", "attachment received"),
    ]);
    let forked_attachment_id = AttachmentId::new("a-transfer-fork").expect("fork attachment id");
    let forked = engine
        .fork_session(SessionFork {
            source_session_id: source_id.clone(),
            source_generation: 1,
            session: new_session(forked_id.as_str(), &sessions_directory),
            conversation,
            attachments: vec![ForkedAttachmentReference {
                source_attachment_id: source_attachment.attachment_id.clone(),
                attachment_id: forked_attachment_id.clone(),
            }],
        })
        .expect("fork attachment session");

    assert_eq!(forked.attachments.len(), 1);
    let forked_attachment = &forked.attachments[0];
    assert_eq!(forked_attachment.attachment_id, forked_attachment_id);
    assert_eq!(forked_attachment.session_id, forked_id);
    assert_eq!(forked_attachment.blob_hash, blob_hash);
    assert_ne!(forked_attachment.agent_readable_path, source_readable_path);
    assert_eq!(
        fs::read(&forked_attachment.agent_readable_path).expect("read fork attachment"),
        bytes
    );
    let ConversationMessage::User(forked_user) = &forked.conversation.messages[0] else {
        panic!("fork first message must be user")
    };
    let UserPart::FileReferences(forked_files) = &forked_user.parts[0] else {
        panic!("fork user message must keep file references")
    };
    assert_eq!(
        forked_files.files[0].readable_path,
        forked_attachment.agent_readable_path
    );

    let impact = engine
        .inspect_session_deletion(&source_id)
        .expect("source delete impact");
    assert_eq!(impact.attachment_count, 1);
    engine
        .delete_session(SessionDeletion {
            session_id: source_id,
            operation_id: "delete-transfer-attachment-source".to_owned(),
            expected_impact: impact,
        })
        .expect("delete source");
    assert_eq!(
        fs::read(&forked_attachment.agent_readable_path)
            .expect("fork attachment survives source deletion"),
        bytes
    );
    assert_eq!(
        engine
            .load_attachments()
            .expect("load remaining attachments")
            .into_iter()
            .map(|attachment| attachment.attachment_id)
            .collect::<Vec<_>>(),
        vec![forked_attachment_id]
    );
}

#[test]
fn deletion_staging_recovers_precommit_and_cleans_postcommit_interruptions() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let restored_id = session_id("s-delete-restore");
    let removed_id = session_id("s-delete-remove");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(restored_id.as_str(), &sessions_directory))
        .expect("create restored fixture");
    engine
        .create_session(new_session(removed_id.as_str(), &sessions_directory))
        .expect("create removed fixture");
    let restored_staging = engine
        .deletion_staging_directory
        .join(format!("{}.interrupted", restored_id));
    fs::rename(
        engine.sessions_directory.join(restored_id.as_str()),
        &restored_staging,
    )
    .expect("stage uncommitted delete");
    let removed_staging = engine
        .deletion_staging_directory
        .join(format!("{}.committed", removed_id));
    fs::rename(
        engine.sessions_directory.join(removed_id.as_str()),
        &removed_staging,
    )
    .expect("stage committed delete");
    engine
        .connection
        .execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            [removed_id.as_str()],
        )
        .expect("commit fixture delete");
    drop(engine);

    let mut reopened = open_engine(&root);
    assert!(
        reopened
            .sessions_directory
            .join(restored_id.as_str())
            .exists()
    );
    assert!(!restored_staging.exists());
    assert!(!removed_staging.exists());
    assert!(
        !reopened
            .sessions_directory
            .join(removed_id.as_str())
            .exists()
    );
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.sessions.len(), 1);
    assert_eq!(recovered.sessions[0].session_id, restored_id);
}

fn seed_session_and_run(engine: &mut StorageEngine, session: &str, run: &str) {
    let new_session = new_session(session, &engine.sessions_directory);
    engine
        .create_session(new_session)
        .expect("create stored session");
    engine
        .connection
        .execute(
            "INSERT INTO inputs (
                input_id, session_id, idempotency_key, user_message_id, state,
                queued_message_json, accepted_at_ms
             ) VALUES (?1, ?2, NULL, ?3, 'committed', NULL, 1001)",
            params![format!("input-{run}"), session, format!("user-{run}")],
        )
        .expect("seed input");
    engine
        .connection
        .execute(
            "INSERT INTO runs (
                run_id, session_id, input_id, attempt, status, cancel_requested,
                error_code, error_message, created_at_ms, started_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, 1, 'accepted', 0, NULL, NULL, 1001, NULL, NULL)",
            params![run, session, format!("input-{run}")],
        )
        .expect("seed run");
}

#[test]
fn model_execution_failure_code_round_trips_without_schema_changes() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "session-model-failure", "run-model-failure");
    let message = "model execution failed before stream establishment (kind=service_unavailable, attempts=4, retries=3, output_observed=false)";
    engine
        .connection
        .execute(
            "UPDATE runs
             SET status = 'failed', error_code = 'model_execution_failed', error_message = ?1,
                 finished_at_ms = 2000
             WHERE run_id = 'run-model-failure'",
            [message],
        )
        .expect("persist model failure");

    let runs = engine.load_runs().expect("load runs");
    let error = runs[0].error.as_ref().expect("model failure");
    assert_eq!(
        error.code,
        assistant_protocol::RuntimeErrorCode::ModelExecutionFailed
    );
    assert_eq!(error.message, message);
}

fn append_request(operation: &str, session: &str, run: &str) -> AppendRequest {
    AppendRequest {
        operation_id: operation.to_owned(),
        session_id: session_id(session),
        run_id: run_id(run),
        messages: vec![
            user_message(&format!("message-{operation}"), "hello"),
            assistant_message(&format!("assistant-{operation}"), "world"),
        ],
        created_at_ms: 2_000,
    }
}

fn assert_recovered_append(root: &TempDir, session: &str) {
    let mut reopened = open_engine(root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.sessions.len(), 1);
    assert_eq!(recovered.sessions[0].message_count, 2);
    assert_eq!(
        recovered.sessions[0].conversation_state,
        StoredConversationState::Available
    );
    assert_eq!(
        reopened
            .load_conversation(&session_id(session))
            .expect("load recovered conversation")
            .messages
            .len(),
        2
    );
    assert_eq!(reopened.staged_append_count().expect("count staged"), 0);
    let reference_count: i64 = reopened
        .connection
        .query_row("SELECT COUNT(*) FROM run_message_refs", [], |row| {
            row.get(0)
        })
        .expect("count refs");
    assert_eq!(reference_count, 2);
}

#[test]
fn initializes_private_database_and_current_schema() {
    let root = tempfile::tempdir().expect("tempdir");
    let engine = open_engine(&root);
    let journal_mode: String = engine
        .connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode");
    let synchronous: i64 = engine
        .connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .expect("synchronous");
    let foreign_keys: i64 = engine
        .connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .expect("foreign keys");
    assert_eq!(journal_mode, "delete");
    assert_eq!(synchronous, 2);
    assert_eq!(foreign_keys, 1);
    let table_count: i64 = engine
        .connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN (
                'sessions', 'inputs', 'runs', 'run_message_refs',
                'pending_tool_exchanges', 'pending_tool_starts', 'body_appends',
                'workspaces', 'session_resources'
             )",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(table_count, 9);

    let database = root.path().join(DATA_DIRECTORY).join(DATABASE_FILE);
    assert_eq!(
        fs::metadata(database)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.path().join(DATA_DIRECTORY).join(SESSIONS_DIRECTORY))
            .expect("sessions metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn child_schema_upgrade_is_idempotent_and_preserves_existing_rows() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-schema-child", "r-schema-child");
    let before: (i64, i64) = engine
        .connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM runs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("count legacy rows");

    super::schema::initialize(&mut engine.connection).expect("repeat schema initialization");
    super::schema::initialize(&mut engine.connection).expect("repeat schema initialization twice");

    let after: (i64, i64, i64) = engine
        .connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM runs),
                    (SELECT COUNT(*) FROM child_tasks)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count rows after compatible schema initialization");
    assert_eq!((after.0, after.1), before);
    assert_eq!(after.2, 0);
}

#[test]
fn child_task_body_is_independent_and_round_trips_all_reliable_steps() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-child", "r-child");
    let child_id = child_task_id("ct-child");
    let created = engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            parent_run_id: run_id("r-child"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-call")
                .expect("parent tool call id"),
            title: "inspect storage".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child prompt".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect("create child task");
    assert_eq!(created.status, ChildTaskStatus::Accepted);
    let task_directory = child_task_directory(
        &engine
            .session_directory(&session_id("s-child"))
            .expect("session directory"),
        &child_id,
    );
    assert!(child_body_path(&task_directory, 1).is_file());

    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            message: raw_user_message("child-user", "inspect"),
            started_at_ms: 2_100,
        })
        .expect("start child task");
    let ConversationMessage::Assistant(tool_assistant) = tool_exchange().remove(0) else {
        unreachable!("tool fixture starts with assistant")
    };
    engine
        .begin_child_tool_exchange(PendingChildToolExchange {
            receipt: ExchangeReceipt::new("child-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            assistant: tool_assistant,
            created_at_ms: 2_200,
        })
        .expect("begin child tool exchange");
    engine
        .mark_child_tool_execution_started(ChildToolExecutionStart {
            receipt: ExchangeReceipt::new("child-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            call_id: assistant_protocol::ToolCallId::new("call-1").expect("call id"),
            started_at_ms: 2_300,
        })
        .expect("mark child tool started");
    engine
        .complete_child_tool_exchange(CompletedChildToolExchange {
            operation_id: "complete-child-tool".to_owned(),
            receipt: ExchangeReceipt::new("child-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            results: tool_results(),
            completed_at_ms: 2_400,
        })
        .expect("complete child tool exchange");
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "settle-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("child-final", "done")],
            final_message_id: Some(MessageId::new("child-final").expect("final id")),
            finished_at_ms: 2_500,
        })
        .expect("settle child task");

    assert_eq!(
        engine
            .load_child_conversation(&session_id("s-child"), &child_id)
            .expect("load child body")
            .messages
            .len(),
        4
    );
    assert!(
        engine
            .load_conversation(&session_id("s-child"))
            .expect("load parent body")
            .messages
            .is_empty()
    );
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover child task");
    assert_eq!(recovered.child_tasks.len(), 1);
    assert_eq!(recovered.child_tasks[0].status, ChildTaskStatus::Completed);
    assert_eq!(recovered.child_tasks[0].message_count, 4);
    assert_eq!(
        recovered.child_tasks[0].conversation_state,
        StoredConversationState::Available
    );
    assert_eq!(recovered.sessions[0].message_count, 0);
    assert_eq!(
        reopened
            .load_child_conversation(&session_id("s-child"), &child_id)
            .expect("load recovered child body")
            .messages
            .len(),
        4
    );
}

#[test]
fn accepted_child_can_fail_before_its_initial_message_is_started() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-child-prestart", "r-child-prestart");
    let child_id = child_task_id("ct-child-prestart");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-prestart"),
            parent_run_id: run_id("r-child-prestart"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-prestart")
                .expect("tool call id"),
            title: "prepare workspace".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect("create accepted child");
    engine
        .request_child_task_cancellation(&session_id("s-child-prestart"), &child_id)
        .expect("record cancellation request");
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "settle-child-prestart".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-prestart"),
            status: ChildTaskStatus::Failed,
            cancel_requested: false,
            error: Some(assistant_protocol::RuntimeErrorInfo::new(
                assistant_protocol::RuntimeErrorCode::Internal,
                "child task workspace could not be prepared",
            )),
            messages: Vec::new(),
            final_message_id: None,
            finished_at_ms: 2_100,
        })
        .expect("settle accepted child as failed");

    let recovered = engine.load_runtime().expect("recover failed child");
    assert_eq!(recovered.child_tasks[0].status, ChildTaskStatus::Failed);
    assert!(recovered.child_tasks[0].cancel_requested);
    assert_eq!(recovered.child_tasks[0].message_count, 0);
    assert!(
        engine
            .load_child_conversation(&session_id("s-child-prestart"), &child_id)
            .expect("empty child body")
            .messages
            .is_empty()
    );
}

#[test]
fn child_task_parent_and_read_ownership_reject_cross_session_access() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-owner", "r-owner");
    engine
        .create_session(new_session("s-other", &engine.sessions_directory))
        .expect("create second session");

    let error = engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_task_id("ct-wrong-owner"),
            session_id: session_id("s-other"),
            parent_run_id: run_id("r-owner"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-wrong")
                .expect("tool call id"),
            title: "wrong owner".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect_err("cross-session parent must be rejected");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);

    let child_id = child_task_id("ct-owner");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-owner"),
            parent_run_id: run_id("r-owner"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-owner")
                .expect("tool call id"),
            title: "owned".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect("create owned child");
    assert_eq!(
        engine
            .load_child_conversation(&session_id("s-other"), &child_id)
            .expect_err("cross-session child read must fail")
            .kind(),
        StoreErrorKind::Conflict
    );
}

#[test]
fn startup_repairs_started_child_tool_exchange_inside_the_child_body_only() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-child-recovery", "r-child-recovery");
    let child_id = child_task_id("ct-child-recovery");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-recovery"),
            parent_run_id: run_id("r-child-recovery"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-recovery")
                .expect("tool call id"),
            title: "recover pending tool".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect("create child");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-child-recovery".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-recovery"),
            message: raw_user_message("child-recovery-user", "inspect"),
            started_at_ms: 2_100,
        })
        .expect("start child");
    let ConversationMessage::Assistant(assistant) = tool_exchange().remove(0) else {
        unreachable!("tool fixture starts with assistant")
    };
    engine
        .begin_child_tool_exchange(PendingChildToolExchange {
            receipt: ExchangeReceipt::new("child-recovery-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-recovery"),
            assistant,
            created_at_ms: 2_200,
        })
        .expect("begin child exchange");
    engine
        .mark_child_tool_execution_started(ChildToolExecutionStart {
            receipt: ExchangeReceipt::new("child-recovery-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child-recovery"),
            call_id: assistant_protocol::ToolCallId::new("call-1").expect("call id"),
            started_at_ms: 2_300,
        })
        .expect("mark child tool started");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("repair child exchange");
    assert_eq!(recovered.child_tasks[0].message_count, 3);
    let conversation = reopened
        .load_child_conversation(&session_id("s-child-recovery"), &child_id)
        .expect("load repaired child conversation");
    assert_eq!(conversation.messages.len(), 3);
    let ConversationMessage::Tool(result) = &conversation.messages[2] else {
        panic!("repaired child exchange must end with a tool result")
    };
    assert_eq!(
        result.result.content,
        ToolResultContent::Text("runtime restarted; tool execution outcome is unknown".to_owned())
    );
    assert!(
        reopened
            .load_conversation(&session_id("s-child-recovery"))
            .expect("load parent conversation")
            .messages
            .is_empty()
    );
    assert_eq!(
        reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_pending_tool_exchanges",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("count child pending exchanges"),
        0
    );
}

#[test]
fn workspace_registration_canonicalizes_soft_deletes_and_restores_the_original_id() {
    let root = TempDir::new().expect("runtime home");
    let user_directory = TempDir::new().expect("user workspace");
    let alias = root.path().join("workspace-alias");
    symlink(user_directory.path(), &alias).expect("workspace alias");
    let mut engine = open_engine(&root);

    let first = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-first"),
            requested_directory: alias.to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register workspace");
    assert_eq!(
        first.user_directory,
        fs::canonicalize(user_directory.path())
            .expect("canonical workspace")
            .to_string_lossy()
    );
    assert!(Path::new(&first.agent_directory).is_dir());
    let default_permissions = PermissionDocument::parse(
        &fs::read(Path::new(&first.agent_directory).join("permissions.json"))
            .expect("default workspace permissions"),
    )
    .expect("valid default workspace permissions");
    assert_eq!(default_permissions.rules.len(), 7);
    assert!(
        default_permissions
            .rules
            .iter()
            .all(|rule| rule.effect == PermissionEffect::Allow)
    );
    assert_eq!(
        default_permissions
            .rules
            .iter()
            .filter(|rule| rule.variants == [assistant_protocol::AgentVariant::Build])
            .count(),
        3
    );
    assert!(default_permissions.rules.iter().all(|rule| {
        matches!(
            &rule.matcher,
            assistant_runtime::PermissionMatcher::File(matcher)
                if matcher.path == first.user_directory
                    && matcher.path_match == assistant_runtime::PathMatch::Recursive
        )
    }));

    let duplicate = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-unused"),
            requested_directory: user_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 200,
        })
        .expect("idempotent workspace");
    assert_eq!(duplicate.workspace_id, first.workspace_id);

    let removed = engine
        .remove_workspace(WorkspaceRemoval {
            workspace_id: first.workspace_id.clone(),
            changed_at_ms: 300,
        })
        .expect("remove workspace");
    assert_eq!(removed.lifecycle, StoredWorkspaceLifecycle::Removed);
    let active_count: i64 = engine
        .connection
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE lifecycle = 'active'",
            [],
            |row| row.get(0),
        )
        .expect("count active workspaces");
    assert_eq!(active_count, 0);
    assert!(Path::new(&removed.agent_directory).is_dir());
    assert!(user_directory.path().is_dir());

    let restored = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-other-unused"),
            requested_directory: alias.to_string_lossy().into_owned(),
            changed_at_ms: 400,
        })
        .expect("restore workspace");
    assert_eq!(restored.workspace_id, first.workspace_id);
    assert_eq!(restored.lifecycle, StoredWorkspaceLifecycle::Active);
}

#[test]
fn workspace_registration_preserves_an_existing_permission_file() {
    let root = TempDir::new().expect("runtime home");
    let user_directory = TempDir::new().expect("user workspace");
    let mut engine = open_engine(&root);
    let workspace = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-custom-permissions"),
            requested_directory: user_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register workspace");
    let permission_path = Path::new(&workspace.agent_directory).join("permissions.json");
    let custom = br#"{"schema_version":1,"rules":[]}
"#;
    fs::write(&permission_path, custom).expect("custom permissions");

    let restored = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-unused-custom"),
            requested_directory: user_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 200,
        })
        .expect("register existing workspace");

    assert_eq!(restored.workspace_id, workspace.workspace_id);
    assert_eq!(
        fs::read(permission_path).expect("custom file remains"),
        custom
    );
}

#[test]
fn runtime_recovery_backfills_only_missing_workspace_permission_files() {
    let root = TempDir::new().expect("runtime home");
    let first_directory = TempDir::new().expect("first workspace");
    let second_directory = TempDir::new().expect("second workspace");
    let mut engine = open_engine(&root);
    let first = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-backfill-default"),
            requested_directory: first_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register first workspace");
    let second = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-backfill-custom"),
            requested_directory: second_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register second workspace");
    let first_path = Path::new(&first.agent_directory).join("permissions.json");
    let second_path = Path::new(&second.agent_directory).join("permissions.json");
    fs::remove_file(&first_path).expect("simulate legacy missing permissions");
    let custom = br#"{"schema_version":1,"rules":[]}
"#;
    fs::write(&second_path, custom).expect("custom permissions");

    let recovered = engine.load_runtime().expect("recover runtime");

    assert_eq!(recovered.workspaces.len(), 2);
    assert!(
        PermissionDocument::parse(&fs::read(first_path).expect("backfilled permissions")).is_ok()
    );
    assert_eq!(fs::read(second_path).expect("custom file remains"), custom);
}

#[test]
fn session_creation_preserves_an_existing_permission_file() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = new_session("s-custom-permissions", &engine.sessions_directory);
    let permission_path =
        Path::new(&session.environment.session_private_directory).join("permissions.json");
    fs::create_dir_all(permission_path.parent().expect("permission parent"))
        .expect("session private directory");
    let custom = br#"{"schema_version":1,"rules":[]}
"#;
    fs::write(&permission_path, custom).expect("custom permissions");

    engine.create_session(session).expect("create session");

    assert_eq!(
        fs::read(permission_path).expect("custom file remains"),
        custom
    );
}

#[test]
fn runtime_recovery_backfills_only_missing_session_permission_files() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    let first = engine
        .create_session(new_session("s-backfill-default", &sessions_directory))
        .expect("create first session");
    let second = engine
        .create_session(new_session("s-backfill-custom", &sessions_directory))
        .expect("create second session");
    let first_path =
        Path::new(&first.environment.session_private_directory).join("permissions.json");
    let second_path =
        Path::new(&second.environment.session_private_directory).join("permissions.json");
    fs::remove_file(&first_path).expect("simulate legacy missing permissions");
    let custom = br#"{"schema_version":1,"rules":[]}
"#;
    fs::write(&second_path, custom).expect("custom permissions");
    drop(engine);

    let _recovered = open_engine(&root);

    assert_default_session_permissions(&first);
    assert_eq!(fs::read(second_path).expect("custom file remains"), custom);
}

#[test]
fn session_workspace_binding_and_legacy_unbound_repair_are_persisted_without_prompt_rewrite() {
    let root = TempDir::new().expect("runtime home");
    let user_directory = TempDir::new().expect("user workspace");
    let mut engine = open_engine(&root);
    let workspace = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-bound"),
            requested_directory: user_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register workspace");

    let mut bound = new_session("s-bound", &engine.sessions_directory);
    bound.environment.workspace_id = Some(workspace.workspace_id.clone());
    bound.environment.working_directory = workspace.user_directory.clone();
    bound.environment.workspace_private_directory = Some(workspace.agent_directory.clone());
    let created = engine.create_session(bound).expect("create bound session");
    assert_eq!(
        created.environment.workspace_id.as_ref(),
        Some(&workspace.workspace_id)
    );
    assert!(Path::new(&created.environment.session_attachment_directory).is_dir());
    assert!(Path::new(&created.environment.session_private_directory).is_dir());
    assert_default_session_permissions(&created);

    let mut second_bound = new_session("s-bound-two", &engine.sessions_directory);
    second_bound.environment.workspace_id = Some(workspace.workspace_id.clone());
    second_bound.environment.working_directory = workspace.user_directory.clone();
    second_bound.environment.workspace_private_directory = Some(workspace.agent_directory.clone());
    let second_created = engine
        .create_session(second_bound)
        .expect("create second bound session");
    assert_eq!(
        created.environment.workspace_private_directory,
        second_created.environment.workspace_private_directory
    );

    let legacy = new_session("s-legacy", &engine.sessions_directory);
    let original_prompt = legacy.system_prompt.clone();
    engine
        .create_session(legacy)
        .expect("create legacy fixture session");
    engine
        .connection
        .execute(
            "DELETE FROM session_resources WHERE session_id = 's-legacy'",
            [],
        )
        .expect("remove resource row to emulate v0.10");
    let legacy_body = body_path(
        &engine
            .session_directory(&session_id("s-legacy"))
            .expect("legacy session directory"),
        1,
    );
    let original_body = fs::read(&legacy_body).expect("read legacy body");
    let original_prompt_json: String = engine
        .connection
        .query_row(
            "SELECT system_prompt_json FROM sessions WHERE session_id = 's-legacy'",
            [],
            |row| row.get(0),
        )
        .expect("read legacy prompt JSON");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    let recovered_bound = recovered
        .sessions
        .iter()
        .find(|session| session.session_id.as_str() == "s-bound")
        .expect("bound session");
    assert_eq!(
        recovered_bound.environment.workspace_id.as_ref(),
        Some(&workspace.workspace_id)
    );
    let repaired = recovered
        .sessions
        .iter()
        .find(|session| session.session_id.as_str() == "s-legacy")
        .expect("legacy session");
    assert_eq!(repaired.environment.workspace_id, None);
    assert_eq!(repaired.system_prompt, original_prompt);
    let repaired_prompt_json: String = reopened
        .connection
        .query_row(
            "SELECT system_prompt_json FROM sessions WHERE session_id = 's-legacy'",
            [],
            |row| row.get(0),
        )
        .expect("read repaired prompt JSON");
    assert_eq!(repaired_prompt_json, original_prompt_json);
    assert_eq!(
        fs::read(legacy_body).expect("read repaired body"),
        original_body
    );
    assert_eq!(
        repaired.environment.working_directory,
        repaired.environment.session_private_directory
    );
    assert!(Path::new(&repaired.environment.session_attachment_directory).is_dir());
    assert!(Path::new(&repaired.environment.session_private_directory).is_dir());
}

#[tokio::test]
async fn worker_round_trips_session_and_shuts_down_explicitly() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = LocalRuntimeStore::open(root.path(), 4)
        .await
        .expect("open local store");
    let sessions_directory = root.path().join(DATA_DIRECTORY).join(SESSIONS_DIRECTORY);
    let created = store
        .create_session(new_session("s-worker", &sessions_directory))
        .await
        .expect("create session");
    assert_eq!(created.message_count, 0);
    assert_eq!(
        store
            .create_session(new_session("s-worker", &sessions_directory))
            .await
            .expect_err("duplicate session")
            .kind(),
        StoreErrorKind::Conflict
    );
    assert!(
        store
            .load_conversation(&created.session_id)
            .await
            .expect("load conversation")
            .messages
            .is_empty()
    );
    assert_eq!(
        store.load_runtime().await.expect("load runtime").sessions,
        vec![created]
    );
    store.shutdown().await.expect("shutdown worker");
    assert_eq!(
        store.load_runtime().await.expect_err("closed store").kind(),
        StoreErrorKind::Unavailable
    );
}

#[test]
fn queued_input_priority_is_non_negative_and_survives_reopen() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = session_id("s-priority");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create session");
    for (suffix, at) in [("one", 1_001), ("two", 1_002), ("three", 1_003)] {
        engine
            .accept_input(NewStoredInput {
                agent_variant: assistant_protocol::AgentVariant::Build,
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                input_id: InputId::new(format!("input-{suffix}")).expect("input id"),
                run_id: run_id(&format!("run-{suffix}")),
                session_id: session.clone(),
                idempotency_key: None,
                message: raw_user_message(&format!("user-{suffix}"), suffix),
                generated_title: None,
                accepted_at_ms: at,
            })
            .expect("accept queued input");
    }
    for selected in ["input-three", "input-two"] {
        engine
            .prioritize_queued_input(QueuePriorityChange {
                session_id: session.clone(),
                input_id: InputId::new(selected).expect("input id"),
            })
            .expect("prioritize queued input");
    }
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    let order = recovered
        .inputs
        .iter()
        .map(|input| (input.input_id.as_str(), input.queue_order))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![("input-two", 0), ("input-three", 1), ("input-one", 2)]
    );
}

#[test]
fn conversation_window_uses_display_boundaries_and_rebuilds_after_append() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = session_id("s-window");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create session");
    for (suffix, at) in [("one", 1_100), ("two", 1_200), ("three", 1_300)] {
        commit_completed_turn(&mut engine, &session, suffix, at);
    }

    let owner = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    let latest = engine
        .load_conversation_window(ConversationWindowRequest {
            owner: owner.clone(),
            generation: 1,
            end: None,
            limit: 2,
        })
        .expect("latest window");
    assert_eq!((latest.start, latest.end, latest.total), (4, 6, 6));
    assert_eq!(latest.conversation.messages.len(), 2);
    assert_eq!(
        conversation::message_id(&latest.conversation.messages[0]).as_str(),
        "user-three"
    );

    let older = engine
        .load_conversation_window(ConversationWindowRequest {
            owner: owner.clone(),
            generation: 1,
            end: Some(latest.start),
            limit: 2,
        })
        .expect("older window");
    assert_eq!((older.start, older.end, older.total), (2, 4, 6));
    assert_eq!(
        conversation::message_id(&older.conversation.messages[0]).as_str(),
        "user-two"
    );

    commit_completed_turn(&mut engine, &session, "four", 1_400);
    let appended = engine
        .load_conversation_window(ConversationWindowRequest {
            owner,
            generation: 1,
            end: None,
            limit: 2,
        })
        .expect("window after append");
    assert_eq!((appended.start, appended.end, appended.total), (6, 8, 8));
    assert_eq!(
        conversation::message_id(&appended.conversation.messages[0]).as_str(),
        "user-four"
    );
}

#[tokio::test]
async fn permission_files_use_fixed_scopes_and_cas_does_not_overwrite_external_edits() {
    let root = tempfile::tempdir().expect("tempdir");
    let user_directory = root.path().join("user-workspace");
    fs::create_dir(&user_directory).expect("user workspace");
    let store = LocalRuntimeStore::open(root.path(), 8)
        .await
        .expect("open local store");
    let workspace = store
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-permission"),
            requested_directory: user_directory.to_string_lossy().into_owned(),
            changed_at_ms: 1,
        })
        .await
        .expect("register workspace");
    let sessions_directory = root.path().join(DATA_DIRECTORY).join(SESSIONS_DIRECTORY);
    let mut session = new_session("s-permission", &sessions_directory);
    session.environment.workspace_id = Some(workspace.workspace_id.clone());
    session.environment.working_directory = workspace.user_directory.clone();
    session.environment.workspace_private_directory = Some(workspace.agent_directory.clone());
    let session = store
        .create_session(session)
        .await
        .expect("create bound session");

    let content = PermissionDocument::empty()
        .render()
        .expect("permission JSON");
    let scopes = [
        PermissionFileScope::Global,
        PermissionFileScope::Workspace(workspace.workspace_id.clone()),
        PermissionFileScope::Session(session.session_id.clone()),
    ];
    for scope in &scopes {
        let current = store
            .load_permission_file(scope)
            .await
            .expect("load current permission");
        if matches!(
            scope,
            PermissionFileScope::Workspace(_) | PermissionFileScope::Session(_)
        ) {
            assert!(matches!(
                current.revision,
                PermissionFileRevision::Content(_)
            ));
        } else {
            assert_eq!(current.revision, PermissionFileRevision::Missing);
        }
        let revision = store
            .replace_permission_file(scope, &current.revision, content.clone())
            .await
            .expect("replace permission file");
        assert!(matches!(revision, PermissionFileRevision::Content(_)));
    }

    let global_path = root.path().join("permissions.json");
    let workspace_path = Path::new(&workspace.agent_directory).join("permissions.json");
    let session_path =
        Path::new(&session.environment.session_private_directory).join("permissions.json");
    assert_eq!(fs::read(&global_path).expect("global file"), content);
    assert_eq!(fs::read(workspace_path).expect("workspace file"), content);
    assert_eq!(fs::read(session_path).expect("session file"), content);

    let original = store
        .load_permission_file(&PermissionFileScope::Global)
        .await
        .expect("load original revision");
    fs::write(&global_path, b"external edit\n").expect("external edit");
    assert_eq!(
        store
            .replace_permission_file(
                &PermissionFileScope::Global,
                &original.revision,
                PermissionDocument::empty().render().expect("replacement"),
            )
            .await
            .expect_err("stale revision must conflict")
            .kind(),
        StoreErrorKind::Conflict
    );
    assert_eq!(
        fs::read(&global_path).expect("external content remains"),
        b"external edit\n"
    );

    fs::set_permissions(&global_path, fs::Permissions::from_mode(0o644))
        .expect("broaden fixture permissions");
    let warning = store
        .load_permission_file(&PermissionFileScope::Global)
        .await
        .expect("broad permissions remain loadable");
    assert!(
        warning
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == PermissionDiagnosticCode::UnsafePermissions })
    );
    store.shutdown().await.expect("shutdown worker");
}

#[test]
fn permission_loader_rejects_symlinks_and_non_regular_files() {
    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("target.json");
    fs::write(&target, b"{}\n").expect("target");
    symlink(&target, root.path().join("permissions.json")).expect("symlink");
    let engine = open_engine(&root);
    assert_eq!(
        engine
            .load_permission_file(&PermissionFileScope::Global)
            .expect_err("symlink must be rejected")
            .kind(),
        StoreErrorKind::InvalidData
    );

    let directory_root = tempfile::tempdir().expect("directory tempdir");
    fs::create_dir(directory_root.path().join("permissions.json"))
        .expect("permission directory fixture");
    let directory_engine = open_engine(&directory_root);
    assert_eq!(
        directory_engine
            .load_permission_file(&PermissionFileScope::Global)
            .expect_err("directory must be rejected")
            .kind(),
        StoreErrorKind::InvalidData
    );
}

#[test]
fn file_references_survive_queued_json_and_conversation_restart_round_trip() {
    let root = TempDir::new().expect("tempdir");
    let mut engine = open_engine(&root);
    let session = session_id("s-file-references");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create session");
    let message = UserMessage {
        id: MessageId::new("message-files").expect("message id"),
        parts: vec![
            UserPart::Text(TextPart {
                id: PartId::new("text-files").expect("part id"),
                text: "compare".to_owned(),
            }),
            UserPart::FileReferences(FileReferencesPart {
                id: PartId::new("references-files").expect("part id"),
                files: vec![
                    FileReference {
                        original_name: "first.pdf".to_owned(),
                        readable_path: "/stable/first.pdf".to_owned(),
                    },
                    FileReference {
                        original_name: "second.xlsx".to_owned(),
                        readable_path: "/stable/second.xlsx".to_owned(),
                    },
                ],
            }),
        ],
    };
    engine
        .accept_input(NewStoredInput {
            agent_variant: assistant_protocol::AgentVariant::Build,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            input_id: InputId::new("input-files").expect("input id"),
            run_id: run_id("run-files"),
            session_id: session.clone(),
            idempotency_key: None,
            message: message.clone(),
            generated_title: None,
            accepted_at_ms: 2_000,
        })
        .expect("accept input");
    assert_eq!(
        engine.load_inputs().expect("load queued input")[0]
            .queued_message
            .as_ref(),
        Some(&message)
    );
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "commit-files".to_owned(),
            input_id: InputId::new("input-files").expect("input id"),
            run_id: run_id("run-files"),
            session_id: session.clone(),
            message: Some(message.clone()),
            created_at_ms: 2_001,
        })
        .expect("commit user message");
    let queued_json: Option<String> = engine
        .connection
        .query_row(
            "SELECT queued_message_json FROM inputs WHERE input_id = 'input-files'",
            [],
            |row| row.get(0),
        )
        .expect("queued JSON state");
    assert_eq!(queued_json, None);
    drop(engine);

    let mut reopened = open_engine(&root);
    reopened.load_runtime().expect("recover runtime");
    assert_eq!(
        reopened
            .load_conversation(&session)
            .expect("load conversation after restart"),
        ConversationSnapshot::new(vec![ConversationMessage::User(message)])
    );
}

#[tokio::test]
async fn worker_panic_is_observable_as_unavailable_and_shutdown_still_joins() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = LocalRuntimeStore::open(root.path(), 4)
        .await
        .expect("open local store");

    assert_eq!(
        store
            .panic_worker_for_test()
            .await
            .expect_err("worker panic must close the reply")
            .kind(),
        StoreErrorKind::Unavailable
    );
    assert_eq!(
        store
            .shutdown()
            .await
            .expect_err("shutdown must observe the failed worker")
            .kind(),
        StoreErrorKind::Unavailable
    );
}

#[test]
fn jsonl_requires_complete_records_unique_ids_and_valid_tool_pairs() {
    let mut expected = vec![
        user_message("message-1", "hello"),
        assistant_message("message-2", "world"),
    ];
    expected.extend(tool_exchange());
    let valid = conversation::encode_messages(&expected).expect("encode");
    assert_eq!(
        conversation::decode(std::io::BufReader::new(valid.as_slice()))
            .expect("decode")
            .messages,
        expected
    );

    let without_newline = &valid[..valid.len() - 1];
    assert_eq!(
        conversation::decode(std::io::BufReader::new(without_newline))
            .expect_err("incomplete record")
            .kind(),
        StoreErrorKind::InvalidData
    );

    let duplicate = conversation::encode_messages(&[
        user_message("message-1", "one"),
        user_message("message-1", "two"),
    ])
    .expect("encode duplicate");
    assert_eq!(
        conversation::decode(std::io::BufReader::new(duplicate.as_slice()))
            .expect_err("duplicate id")
            .kind(),
        StoreErrorKind::InvalidData
    );
}

#[test]
fn staged_append_recovers_from_each_durable_interruption_point() {
    for phase in ["staged", "partial", "written", "finalized"] {
        let root = tempfile::tempdir().expect("tempdir");
        let mut engine = open_engine(&root);
        seed_session_and_run(&mut engine, "s-recovery", "r-recovery");
        let request = append_request("operation-recovery", "s-recovery", "r-recovery");

        match phase {
            "staged" => engine.stage_append(request).expect("stage append"),
            "partial" => {
                engine.stage_append(request).expect("stage append");
                let (base, payload): (i64, Vec<u8>) = engine
                    .connection
                    .query_row(
                        "SELECT base_byte_length, payload FROM body_appends WHERE operation_id = 'operation-recovery'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .expect("read staged payload");
                let path = body_path(
                    &engine
                        .session_directory(&session_id("s-recovery"))
                        .expect("session directory"),
                    1,
                );
                let mut file = OpenOptions::new()
                    .write(true)
                    .open(path)
                    .expect("open body");
                assert_eq!(base, 0);
                file.write_all(&payload[..payload.len() / 2])
                    .expect("write partial payload");
                file.sync_data().expect("sync partial payload");
            }
            "written" => {
                engine.stage_append(request).expect("stage append");
                engine
                    .write_staged_append("operation-recovery")
                    .expect("write staged append");
            }
            "finalized" => engine.append_messages(request).expect("append messages"),
            _ => unreachable!("known phase"),
        }
        drop(engine);

        assert_recovered_append(&root, "s-recovery");
    }
}

#[test]
fn completed_tool_exchange_enters_body_as_one_batch_and_clears_pending() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-tool", "r-tool");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 WHERE run_id = 'r-tool'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange("s-tool", "r-tool", "receipt-tool"))
        .expect("begin tool exchange");
    start_tool(&mut engine, "s-tool", "r-tool", "receipt-tool");
    let pending_state: String = engine
        .connection
        .query_row(
            "SELECT state FROM pending_tool_exchanges WHERE receipt_id = 'receipt-tool'",
            [],
            |row| row.get(0),
        )
        .expect("query pending state");
    assert_eq!(pending_state, "begun");

    engine
        .complete_tool_exchange(CompletedToolExchange {
            operation_id: "append-tool".to_owned(),
            receipt: ExchangeReceipt::new("receipt-tool").expect("receipt"),
            session_id: session_id("s-tool"),
            run_id: run_id("r-tool"),
            results: tool_results(),
            completed_at_ms: 2_500,
        })
        .expect("complete tool exchange");

    let pending_count: i64 = engine
        .connection
        .query_row("SELECT COUNT(*) FROM pending_tool_exchanges", [], |row| {
            row.get(0)
        })
        .expect("count pending");
    assert_eq!(pending_count, 0);
    let started_count: i64 = engine
        .connection
        .query_row("SELECT COUNT(*) FROM pending_tool_starts", [], |row| {
            row.get(0)
        })
        .expect("count pending starts");
    assert_eq!(started_count, 0);
    let conversation = engine
        .load_conversation(&session_id("s-tool"))
        .expect("load conversation");
    assert_eq!(conversation.messages, tool_exchange());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM run_message_refs", [], |row| row
                .get::<_, i64>(0))
            .expect("count refs"),
        2
    );
}

#[test]
fn startup_repairs_unstarted_tool_as_not_executed() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-unstarted", "r-unstarted");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 WHERE run_id = 'r-unstarted'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange(
            "s-unstarted",
            "r-unstarted",
            "receipt-unstarted",
        ))
        .expect("begin tool exchange");
    drop(engine);

    let mut reopened = open_engine(&root);
    reopened.load_runtime().expect("recover unstarted exchange");
    let conversation = reopened
        .load_conversation(&session_id("s-unstarted"))
        .expect("load repaired conversation");
    let ConversationMessage::Tool(result) = &conversation.messages[1] else {
        panic!("repaired batch must end with a tool result")
    };
    assert_eq!(result.result.status, ToolResultStatus::Error);
    assert_eq!(
        result.result.content,
        ToolResultContent::Text("runtime restarted before tool execution started".to_owned())
    );
}

#[test]
fn pending_tool_exchange_prevents_run_terminal_settlement() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-pending", "r-pending");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 WHERE run_id = 'r-pending'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange(
            "s-pending",
            "r-pending",
            "receipt-pending",
        ))
        .expect("begin tool exchange");

    let error = engine
        .settle_run(StoredRunSettlement {
            operation_id: "settle-pending".to_owned(),
            run_id: run_id("r-pending"),
            session_id: session_id("s-pending"),
            status: assistant_protocol::RunStatus::Failed,
            cancel_requested: false,
            error: None,
            messages: Vec::new(),
            finished_at_ms: 3_000,
        })
        .expect_err("pending exchange must block terminal settlement");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT status FROM runs WHERE run_id = 'r-pending'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("query run status"),
        "running"
    );
}

#[test]
fn startup_repairs_begun_tool_exchange_with_unknown_results_without_reexecution() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-begun", "r-begun");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 WHERE run_id = 'r-begun'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange("s-begun", "r-begun", "receipt-begun"))
        .expect("begin tool exchange");
    start_tool(&mut engine, "s-begun", "r-begun", "receipt-begun");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover begun exchange");
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Interrupted
    );
    let conversation = reopened
        .load_conversation(&session_id("s-begun"))
        .expect("load repaired conversation");
    assert_eq!(conversation.messages.len(), 2);
    let ConversationMessage::Tool(result) = &conversation.messages[1] else {
        panic!("repaired batch must end with a tool result")
    };
    assert_eq!(result.result.status, ToolResultStatus::Error);
    assert_eq!(
        result.result.content,
        ToolResultContent::Text("runtime restarted; tool execution outcome is unknown".to_owned())
    );
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM pending_tool_exchanges", [], |row| row
                .get::<_, i64>(0))
            .expect("count pending"),
        0
    );
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM pending_tool_starts", [], |row| row
                .get::<_, i64>(0))
            .expect("count pending starts"),
        0
    );
}

#[test]
fn startup_rebuilds_parent_delegate_result_from_completed_child() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-delegate-complete", "r-delegate-complete");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500
             WHERE run_id = 'r-delegate-complete'",
            [],
        )
        .expect("mark parent running");
    engine
        .begin_tool_exchange(pending_delegate_exchange(
            "s-delegate-complete",
            "r-delegate-complete",
            "receipt-delegate-complete",
        ))
        .expect("begin delegate exchange");
    start_tool(
        &mut engine,
        "s-delegate-complete",
        "r-delegate-complete",
        "receipt-delegate-complete",
    );
    let child_id = child_task_id("ct-delegate-complete");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-delegate-complete"),
            parent_run_id: run_id("r-delegate-complete"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("call-1").expect("call id"),
            title: "recover completed child".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_100,
        })
        .expect("create child");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-delegate-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-delegate-complete"),
            message: raw_user_message("delegate-child-user", "inspect"),
            started_at_ms: 2_200,
        })
        .expect("start child");
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "settle-delegate-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-delegate-complete"),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message(
                "delegate-child-final",
                "recovered answer",
            )],
            final_message_id: Some(MessageId::new("delegate-child-final").expect("message id")),
            finished_at_ms: 2_300,
        })
        .expect("settle child");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.child_tasks[0].status, ChildTaskStatus::Completed);
    assert_eq!(recovered.runs[0].status, RunStatus::Interrupted);
    let conversation = reopened
        .load_conversation(&session_id("s-delegate-complete"))
        .expect("parent conversation");
    let ConversationMessage::Tool(tool) = &conversation.messages[1] else {
        panic!("delegate recovery must append one tool result");
    };
    assert_eq!(tool.result.status, ToolResultStatus::Success);
    let ToolResultContent::Json(content) = &tool.result.content else {
        panic!("completed child result must remain structured");
    };
    assert_eq!(content["task_id"], child_id.as_str());
    assert_eq!(content["result"], "recovered answer");
}

#[test]
fn startup_interrupts_running_child_before_rebuilding_parent_result() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-delegate-running", "r-delegate-running");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500
             WHERE run_id = 'r-delegate-running'",
            [],
        )
        .expect("mark parent running");
    engine
        .begin_tool_exchange(pending_delegate_exchange(
            "s-delegate-running",
            "r-delegate-running",
            "receipt-delegate-running",
        ))
        .expect("begin delegate exchange");
    start_tool(
        &mut engine,
        "s-delegate-running",
        "r-delegate-running",
        "receipt-delegate-running",
    );
    let child_id = child_task_id("ct-delegate-running");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-delegate-running"),
            parent_run_id: run_id("r-delegate-running"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("call-1").expect("call id"),
            title: "recover running child".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_100,
        })
        .expect("create child");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-running-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-delegate-running"),
            message: raw_user_message("running-child-user", "inspect"),
            started_at_ms: 2_200,
        })
        .expect("start child");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(
        recovered.child_tasks[0].status,
        ChildTaskStatus::Interrupted
    );
    assert_eq!(recovered.runs[0].status, RunStatus::Interrupted);
    let conversation = reopened
        .load_conversation(&session_id("s-delegate-running"))
        .expect("parent conversation");
    let ConversationMessage::Tool(tool) = &conversation.messages[1] else {
        panic!("delegate recovery must append one tool result");
    };
    let ToolResultContent::Json(content) = &tool.result.content else {
        panic!("interrupted child result must remain structured");
    };
    assert_eq!(content["error"]["details"]["task_id"], child_id.as_str());
    assert_eq!(content["error"]["details"]["code"], "interrupted");
}

#[test]
fn startup_commits_ready_tool_exchange_with_its_recorded_results() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-ready", "r-ready");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 WHERE run_id = 'r-ready'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange("s-ready", "r-ready", "receipt-ready"))
        .expect("begin tool exchange");
    start_tool(&mut engine, "s-ready", "r-ready", "receipt-ready");
    engine
        .connection
        .execute(
            "UPDATE pending_tool_exchanges SET state = 'ready', results_json = ?1
             WHERE receipt_id = 'receipt-ready'",
            [serde_json::to_string(&tool_results()).expect("encode results")],
        )
        .expect("mark ready fixture");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover ready exchange");
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Interrupted
    );
    assert_eq!(
        reopened
            .load_conversation(&session_id("s-ready"))
            .expect("load recovered conversation")
            .messages,
        tool_exchange()
    );
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM pending_tool_exchanges", [], |row| row
                .get::<_, i64>(0))
            .expect("count pending"),
        0
    );
    assert_eq!(
        reopened
            .connection
            .query_row("SELECT COUNT(*) FROM pending_tool_starts", [], |row| row
                .get::<_, i64>(0))
            .expect("count pending starts"),
        0
    );
}

#[test]
fn startup_marks_every_nonterminal_run_interrupted_without_resuming_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-interrupted", "r-interrupted");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1002
             WHERE run_id = 'r-interrupted'",
            [],
        )
        .expect("mark run running");

    let recovered = engine.load_runtime().expect("recover runtime");
    assert_eq!(recovered.runs.len(), 1);
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Interrupted
    );
    assert!(recovered.runs[0].finished_at_ms.is_some());
}

#[test]
fn startup_finishes_staged_run_start_before_marking_it_interrupted() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-start", "r-start");
    engine
        .connection
        .execute(
            "UPDATE inputs
             SET state = 'queued', queued_message_json = '{}'
             WHERE input_id = 'input-r-start'",
            [],
        )
        .expect("restore queued input fixture");
    engine
        .stage_append_for(
            AppendRequest {
                operation_id: "operation-start".to_owned(),
                session_id: session_id("s-start"),
                run_id: run_id("r-start"),
                messages: vec![user_message("user-r-start", "hello")],
                created_at_ms: 2_000,
            },
            AppendPurpose::UserMessage,
        )
        .expect("stage run start");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover staged run start");
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Interrupted
    );
    let input_state: String = reopened
        .connection
        .query_row(
            "SELECT state FROM inputs WHERE input_id = 'input-r-start'",
            [],
            |row| row.get(0),
        )
        .expect("query recovered input");
    assert_eq!(input_state, "committed");
    assert_eq!(recovered.sessions[0].message_count, 1);
    assert_eq!(recovered.runs[0].message_ids.len(), 1);
}

#[test]
fn startup_finishes_staged_terminal_message_and_run_status_together() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-settle", "r-settle");
    engine
        .append_messages(AppendRequest {
            operation_id: "operation-user".to_owned(),
            session_id: session_id("s-settle"),
            run_id: run_id("r-settle"),
            messages: vec![user_message("user-r-settle", "hello")],
            created_at_ms: 2_000,
        })
        .expect("append user fixture");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 2000
             WHERE run_id = 'r-settle'",
            [],
        )
        .expect("mark fixture running");
    engine
        .stage_append_for(
            AppendRequest {
                operation_id: "operation-settle".to_owned(),
                session_id: session_id("s-settle"),
                run_id: run_id("r-settle"),
                messages: vec![assistant_message("assistant-r-settle", "done")],
                created_at_ms: 3_000,
            },
            AppendPurpose::RunSettlement {
                status: assistant_protocol::RunStatus::Completed,
                cancel_requested: false,
                error: None,
            },
        )
        .expect("stage run settlement");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover staged settlement");
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(recovered.sessions[0].message_count, 2);
    assert_eq!(recovered.runs[0].message_ids.len(), 2);
    assert_eq!(
        reopened
            .load_conversation(&session_id("s-settle"))
            .expect("load settled conversation")
            .messages
            .len(),
        2
    );
}

#[test]
fn conflicting_append_tail_isolated_to_its_session() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-broken", "r-broken");
    let healthy = new_session("s-healthy", &engine.sessions_directory);
    engine
        .create_session(healthy)
        .expect("create healthy session");
    engine
        .stage_append(append_request("operation-broken", "s-broken", "r-broken"))
        .expect("stage append");
    let broken_path = body_path(
        &engine
            .session_directory(&session_id("s-broken"))
            .expect("session directory"),
        1,
    );
    let mut file = OpenOptions::new()
        .append(true)
        .open(broken_path)
        .expect("open broken body");
    file.write_all(b"conflicting bytes")
        .expect("write conflicting tail");
    file.sync_data().expect("sync conflicting tail");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    let broken = recovered
        .sessions
        .iter()
        .find(|session| session.session_id.as_str() == "s-broken")
        .expect("broken session");
    let healthy = recovered
        .sessions
        .iter()
        .find(|session| session.session_id.as_str() == "s-healthy")
        .expect("healthy session");
    assert_eq!(
        broken.conversation_state,
        StoredConversationState::Unavailable
    );
    assert_eq!(
        healthy.conversation_state,
        StoredConversationState::Available
    );
    assert_eq!(
        reopened
            .load_conversation(&broken.session_id)
            .expect_err("broken body remains unavailable")
            .kind(),
        StoreErrorKind::InvalidData
    );
    assert!(
        reopened
            .load_conversation(&healthy.session_id)
            .expect("healthy conversation")
            .messages
            .is_empty()
    );
    assert_eq!(reopened.staged_append_count().expect("staged count"), 1);
}

#[test]
fn generation_switch_keeps_old_authority_until_sqlite_commit() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-rewrite", "r-rewrite");
    engine
        .append_messages(append_request(
            "operation-original",
            "s-rewrite",
            "r-rewrite",
        ))
        .expect("append original");
    let replacement = ConversationSnapshot::new(vec![user_message("message-new", "replacement")]);
    let plan = engine
        .begin_replacement(session_id("s-rewrite"), replacement.clone())
        .expect("write replacement generation");
    drop(engine);

    let reopened = open_engine(&root);
    assert_eq!(
        reopened
            .load_conversation(&session_id("s-rewrite"))
            .expect("old authority")
            .messages
            .len(),
        2
    );
    drop(reopened);

    let mut switched = open_engine(&root);
    switch_generation_without_cleanup(&mut switched.connection, &plan);
    drop(switched);
    let authoritative = open_engine(&root);
    assert_eq!(
        authoritative
            .load_conversation(&session_id("s-rewrite"))
            .expect("new authority"),
        replacement
    );
}

#[test]
fn committed_generation_replaces_message_count_and_removes_old_body() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-commit", "r-commit");
    engine
        .append_messages(append_request("operation-old", "s-commit", "r-commit"))
        .expect("append original");
    let replacement = ConversationSnapshot::new(vec![user_message("message-only", "replacement")]);
    let plan = engine
        .begin_replacement(session_id("s-commit"), replacement.clone())
        .expect("begin replacement");
    let old_path = body_path(
        &engine
            .session_directory(&session_id("s-commit"))
            .expect("session directory"),
        plan.previous_generation,
    );
    engine
        .commit_replacement(&plan)
        .expect("commit replacement");
    assert!(!old_path.exists());
    let recovered = engine.load_runtime().expect("load runtime");
    assert_eq!(recovered.sessions[0].body_generation, plan.new_generation);
    assert_eq!(recovered.sessions[0].message_count, 1);
    assert_eq!(
        engine
            .load_conversation(&session_id("s-commit"))
            .expect("load replacement"),
        replacement
    );
}

#[test]
fn active_run_context_replacement_switches_generation_without_rewriting_run_relations() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-compact-run", "r-compact-run");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_100 WHERE run_id = 'r-compact-run'",
            [],
        )
        .expect("activate run");
    engine
        .append_messages(append_request(
            "operation-compact-run",
            "s-compact-run",
            "r-compact-run",
        ))
        .expect("append original");
    let replacement = ConversationSnapshot::new(vec![user_message(
        "message-compact-run",
        "summary replacement",
    )]);

    engine
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::Run {
                session_id: session_id("s-compact-run"),
                run_id: run_id("r-compact-run"),
            },
            conversation: replacement.clone(),
            changed_at_ms: 3_000,
        })
        .expect("replace active run context");

    let generation: i64 = engine
        .connection
        .query_row(
            "SELECT body_generation FROM sessions WHERE session_id = 's-compact-run'",
            [],
            |row| row.get(0),
        )
        .expect("session generation");
    let status: String = engine
        .connection
        .query_row(
            "SELECT status FROM runs WHERE run_id = 'r-compact-run'",
            [],
            |row| row.get(0),
        )
        .expect("run status");
    assert_eq!(generation, 2);
    assert_eq!(status, "running");
    assert_eq!(
        engine
            .load_conversation(&session_id("s-compact-run"))
            .expect("replacement conversation"),
        replacement
    );
}

#[test]
fn running_child_context_replacement_switches_only_child_generation() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-compact-child", "r-compact-child");
    let child_id = child_task_id("ct-compact-child");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-compact-child"),
            parent_run_id: run_id("r-compact-child"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-compact")
                .expect("call id"),
            title: "compact child".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child prompt".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .expect("create child");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-compact-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-compact-child"),
            message: raw_user_message("child-original", "long child task"),
            started_at_ms: 2_100,
        })
        .expect("start child");
    let replacement = ConversationSnapshot::new(vec![user_message(
        "child-replacement",
        "summary replacement",
    )]);

    engine
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::ChildTask {
                session_id: session_id("s-compact-child"),
                child_task_id: child_id.clone(),
            },
            conversation: replacement.clone(),
            changed_at_ms: 3_000,
        })
        .expect("replace child context");

    let tasks = engine.load_child_tasks().expect("load child tasks");
    assert_eq!(tasks[0].body_generation, 2);
    assert_eq!(tasks[0].status, ChildTaskStatus::Running);
    assert_eq!(
        engine
            .load_child_conversation(&session_id("s-compact-child"), &child_id)
            .expect("child replacement"),
        replacement
    );
}

fn switch_generation_without_cleanup(connection: &mut Connection, plan: &ReplacementPlan) {
    connection
        .execute(
            "UPDATE sessions
             SET body_generation = ?1, message_count = ?2
             WHERE session_id = ?3 AND body_generation = ?4",
            params![
                i64::try_from(plan.new_generation).expect("new generation"),
                i64::try_from(plan.message_count).expect("message count"),
                plan.session_id.as_str(),
                i64::try_from(plan.previous_generation).expect("old generation"),
            ],
        )
        .expect("switch generation");
}

#[test]
fn history_rewrite_switches_generation_and_removes_tail_relations_atomically() {
    let root = TempDir::new().expect("tempdir");
    let mut engine = open_engine(&root);
    let session = session_id("s-rewrite");
    let stored_session = new_session(session.as_str(), &engine.sessions_directory);
    engine
        .create_session(stored_session)
        .expect("create session");

    for (input, run, user, assistant, at) in [
        ("input-one", "run-one", "user-one", "assistant-one", 1_100),
        ("input-two", "run-two", "user-two", "assistant-two", 1_200),
    ] {
        let user_message = raw_user_message(user, user);
        engine
            .accept_input(NewStoredInput {
                agent_variant: assistant_protocol::AgentVariant::Build,
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                input_id: InputId::new(input).expect("input id"),
                run_id: run_id(run),
                session_id: session.clone(),
                idempotency_key: None,
                message: user_message.clone(),
                generated_title: None,
                accepted_at_ms: at,
            })
            .expect("accept input");
        engine
            .commit_user_message(UserMessageCommit {
                operation_id: format!("start-{run}"),
                input_id: InputId::new(input).expect("input id"),
                run_id: run_id(run),
                session_id: session.clone(),
                message: Some(user_message),
                created_at_ms: at + 1,
            })
            .expect("commit user");
        engine
            .settle_run(StoredRunSettlement {
                operation_id: format!("settle-{run}"),
                run_id: run_id(run),
                session_id: session.clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message(assistant, assistant)],
                finished_at_ms: at + 2,
            })
            .expect("settle run");
    }

    let removed_child_id = child_task_id("ct-rewrite-tail");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: removed_child_id.clone(),
            session_id: session.clone(),
            parent_run_id: run_id("run-two"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-rewrite-tail")
                .expect("tool call id"),
            title: "removed tail child".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 1_300,
        })
        .expect("create tail child");
    let removed_child_directory = child_task_directory(
        &engine
            .session_directory(&session)
            .expect("session directory"),
        &removed_child_id,
    );
    assert!(removed_child_directory.is_dir());

    let replacement_user = raw_user_message("user-replacement", "replacement");
    let result = engine
        .rewrite_from_user(ConversationRewrite {
            session_id: session.clone(),
            target_user_message_id: MessageId::new("user-one").expect("message id"),
            conversation: ConversationSnapshot::new(vec![ConversationMessage::User(
                replacement_user.clone(),
            )]),
            input: NewStoredInput {
                agent_variant: assistant_protocol::AgentVariant::Build,
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                input_id: InputId::new("input-replacement").expect("input id"),
                run_id: run_id("run-replacement"),
                session_id: session.clone(),
                idempotency_key: Some(IdempotencyKey::new("rewrite-1").expect("key")),
                message: replacement_user,
                generated_title: None,
                accepted_at_ms: 2_000,
            },
            changed_at_ms: 2_000,
        })
        .expect("rewrite history");

    assert_eq!(
        result.input.state,
        assistant_runtime::StoredInputState::Committed
    );
    assert_eq!(result.run.status, RunStatus::Accepted);
    assert_eq!(engine.session_generation(&session).expect("generation"), 2);
    assert_eq!(
        engine.load_conversation(&session).expect("conversation"),
        ConversationSnapshot::new(vec![user_message("user-replacement", "replacement")])
    );
    let recovered = engine.load_runtime().expect("restart recovery");
    assert_eq!(recovered.inputs.len(), 1);
    assert_eq!(recovered.inputs[0].input_id.as_str(), "input-replacement");
    assert_eq!(recovered.runs.len(), 1);
    assert_eq!(recovered.runs[0].run_id.as_str(), "run-replacement");
    assert_eq!(recovered.runs[0].status, RunStatus::Interrupted);
    let old_body = body_path(
        &engine
            .session_directory(&session)
            .expect("session directory"),
        1,
    );
    assert!(!old_body.exists());
    assert!(!removed_child_directory.exists());
}

#[test]
fn archive_and_model_changes_are_persisted_and_recheck_idle_state() {
    let root = TempDir::new().expect("tempdir");
    let mut engine = open_engine(&root);
    let session = session_id("s-lifecycle");
    let stored_session = new_session(session.as_str(), &engine.sessions_directory);
    engine
        .create_session(stored_session)
        .expect("create session");
    engine
        .set_session_archive(ArchiveChange {
            session_id: session.clone(),
            archived: true,
            changed_at_ms: 2_000,
        })
        .expect("archive");
    assert!(matches!(
        engine.set_session_model(ModelChange {
            session_id: session.clone(),
            model_key: ModelKey::new("other-model").expect("model key"),
            changed_at_ms: 2_001,
        }),
        Err(error) if error.kind() == StoreErrorKind::Conflict
    ));
    engine
        .set_session_archive(ArchiveChange {
            session_id: session.clone(),
            archived: false,
            changed_at_ms: 2_002,
        })
        .expect("restore");
    engine
        .set_session_model(ModelChange {
            session_id: session.clone(),
            model_key: ModelKey::new("other-model").expect("model key"),
            changed_at_ms: 2_003,
        })
        .expect("model change");

    let queued_message = raw_user_message("queued-user", "queued");
    engine
        .accept_input(NewStoredInput {
            agent_variant: assistant_protocol::AgentVariant::Build,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            input_id: InputId::new("queued-input").expect("input id"),
            run_id: run_id("queued-run"),
            session_id: session.clone(),
            idempotency_key: None,
            message: queued_message,
            generated_title: None,
            accepted_at_ms: 2_004,
        })
        .expect("queued input");
    engine
        .set_session_variant(VariantChange {
            session_id: session.clone(),
            variant: assistant_protocol::AgentVariant::Plan,
            changed_at_ms: 2_005,
        })
        .expect("variant change while run is active");
    engine
        .set_session_approval_mode(ApprovalModeChange {
            session_id: session.clone(),
            approval_mode: assistant_protocol::ApprovalMode::Auto,
            changed_at_ms: 2_006,
        })
        .expect("approval mode change while run is active");
    assert!(matches!(
        engine.set_session_archive(ArchiveChange {
            session_id: session.clone(),
            archived: true,
            changed_at_ms: 2_007,
        }),
        Err(error) if error.kind() == StoreErrorKind::Conflict
    ));
    let recovered = engine.load_runtime().expect("load runtime");
    assert_eq!(
        recovered.sessions[0].lifecycle,
        StoredSessionLifecycle::Active
    );
    assert_eq!(recovered.sessions[0].model_key.as_str(), "other-model");
    assert_eq!(
        recovered.sessions[0].current_variant,
        assistant_protocol::AgentVariant::Plan
    );
    assert_eq!(
        recovered.sessions[0].approval_mode,
        assistant_protocol::ApprovalMode::Auto
    );
    assert_eq!(
        recovered.runs[0].agent_variant,
        assistant_protocol::AgentVariant::Build
    );
    assert_eq!(
        recovered.runs[0].approval_mode,
        assistant_protocol::ApprovalMode::Ask
    );
}

#[test]
fn attachment_upload_uses_name_and_bytes_for_blob_identity_and_repairs_known_views() {
    let root = TempDir::new().expect("tempdir");
    let mut engine = open_engine(&root);
    let session = session_id("s-attachment");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create session");
    let bytes = b"stable attachment content";
    let original_name = "需求说明.txt";
    let hash = crate::attachment_hash::digest_bytes(original_name, bytes);
    let first_staging = engine.upload_staging_directory.join("first.part");
    fs::write(&first_staging, bytes).expect("write staging");
    let first = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-first").expect("attachment id"),
            session_id: session.clone(),
            original_name: original_name.to_owned(),
            staging_path: first_staging.to_string_lossy().into_owned(),
            blob_hash: hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_000,
        })
        .expect("upload attachment");
    assert_eq!(first.state, StoredAttachmentState::Ready);
    assert!(
        fs::symlink_metadata(&first.agent_readable_path)
            .expect("stable view")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::canonicalize(&first.agent_readable_path)
            .expect("canonical attachment blob")
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("txt")
    );

    let duplicate_staging = engine.upload_staging_directory.join("duplicate.part");
    fs::write(&duplicate_staging, bytes).expect("write duplicate staging");
    let duplicate = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-duplicate").expect("attachment id"),
            session_id: session.clone(),
            original_name: original_name.to_owned(),
            staging_path: duplicate_staging.to_string_lossy().into_owned(),
            blob_hash: hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_001,
        })
        .expect("idempotent retry");
    assert_eq!(duplicate.attachment_id, first.attachment_id);
    assert!(!duplicate_staging.exists());

    let different_name = "different.txt";
    let different_hash = crate::attachment_hash::digest_bytes(different_name, bytes);
    assert_ne!(different_hash, hash);
    let different_staging = engine.upload_staging_directory.join("different.part");
    fs::write(&different_staging, bytes).expect("write differently named staging");
    let differently_named = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-different").expect("attachment id"),
            session_id: session,
            original_name: different_name.to_owned(),
            staging_path: different_staging.to_string_lossy().into_owned(),
            blob_hash: different_hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_002,
        })
        .expect("upload same bytes under another name");
    assert_ne!(differently_named.attachment_id, first.attachment_id);

    let reused_staging = engine.upload_staging_directory.join("reused.part");
    fs::write(&reused_staging, bytes).expect("write reused staging");
    let other_session = session_id("s-attachment-other");
    engine
        .create_session(new_session(
            other_session.as_str(),
            &engine.sessions_directory,
        ))
        .expect("create other session");
    let reused = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-reused").expect("attachment id"),
            session_id: other_session,
            original_name: original_name.to_owned(),
            staging_path: reused_staging.to_string_lossy().into_owned(),
            blob_hash: hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_003,
        })
        .expect("reuse attachment blob");
    assert_ne!(reused.attachment_id, first.attachment_id);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM attachment_blobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("blob count"),
        2
    );

    fs::remove_file(&first.agent_readable_path).expect("remove repairable view");
    drop(engine);
    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover attachments");
    assert_eq!(recovered.attachments.len(), 3);
    assert_eq!(recovered.sessions.len(), 2);
    assert!(
        recovered
            .attachments
            .iter()
            .all(|attachment| attachment.state == StoredAttachmentState::Ready)
    );
    assert!(Path::new(&first.agent_readable_path).is_symlink());

    let blob = root
        .path()
        .join("data")
        .join(super::attachment_io::blob_relative_path(
            &hash,
            original_name,
        ));
    fs::remove_file(blob).expect("remove blob for unavailable recovery");
    drop(reopened);
    let mut reopened = open_engine(&root);
    let recovered = reopened
        .load_runtime()
        .expect("recover unavailable attachment");
    let unavailable = recovered
        .attachments
        .iter()
        .filter(|attachment| attachment.state == StoredAttachmentState::Unavailable)
        .count();
    let ready = recovered
        .attachments
        .iter()
        .filter(|attachment| attachment.state == StoredAttachmentState::Ready)
        .count();
    assert_eq!(unavailable, 2);
    assert_eq!(ready, 1);
    assert_eq!(
        recovered
            .attachments
            .iter()
            .find(|attachment| attachment.attachment_id == differently_named.attachment_id)
            .expect("differently named attachment")
            .state,
        StoredAttachmentState::Ready
    );
}

#[test]
fn attachment_recovery_migrates_extensionless_blobs_and_known_views() {
    let root = TempDir::new().expect("tempdir");
    let mut engine = open_engine(&root);
    let session = session_id("s-legacy-attachment");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create session");
    let bytes = b"legacy image bytes";
    let original_name = "legacy-image.png";
    let hash = crate::attachment_hash::digest_bytes(original_name, bytes);
    let staging = engine.upload_staging_directory.join("legacy.part");
    fs::write(&staging, bytes).expect("write staging");
    let attachment = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-legacy").expect("attachment id"),
            session_id: session,
            original_name: original_name.to_owned(),
            staging_path: staging.to_string_lossy().into_owned(),
            blob_hash: hash.clone(),
            size_bytes: bytes.len() as u64,
            created_at_ms: 2_000,
        })
        .expect("upload attachment");

    let data_directory = root.path().join("data");
    let current_relative = super::attachment_io::blob_relative_path(&hash, original_name);
    let legacy_relative = super::attachment_io::legacy_blob_relative_path(&hash);
    fs::rename(
        data_directory.join(&current_relative),
        data_directory.join(&legacy_relative),
    )
    .expect("restore legacy blob path");
    engine
        .connection
        .execute(
            "UPDATE attachment_blobs SET relative_path = ?2 WHERE blob_hash = ?1",
            params![hash, legacy_relative.to_string_lossy().as_ref(),],
        )
        .expect("restore legacy metadata");
    fs::remove_file(&attachment.agent_readable_path).expect("remove current stable view");
    symlink(
        Path::new("../../../../").join(&legacy_relative),
        &attachment.agent_readable_path,
    )
    .expect("restore legacy stable view");
    drop(engine);

    let reopened = open_engine(&root);
    assert!(!data_directory.join(&legacy_relative).exists());
    assert_eq!(
        fs::read(data_directory.join(&current_relative)).expect("read migrated blob"),
        bytes
    );
    assert_eq!(
        fs::read_link(&attachment.agent_readable_path).expect("read migrated stable view"),
        super::attachment_io::relative_blob_link(&hash, original_name)
    );
    assert_eq!(
        reopened
            .connection
            .query_row(
                "SELECT relative_path FROM attachment_blobs WHERE blob_hash = ?1",
                [hash.as_str()],
                |row| row.get::<_, String>(0),
            )
            .expect("migrated blob metadata"),
        current_relative.to_string_lossy()
    );
}
