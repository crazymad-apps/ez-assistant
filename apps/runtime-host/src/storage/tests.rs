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
    AttachmentId, IdempotencyKey, InputId, ModelKey, RunId, RunStatus, SessionId, WorkspaceId,
};
use assistant_runtime::{
    ArchiveChange, CompletedToolExchange, ConversationRewrite, ModelChange, NewAttachmentUpload,
    NewStoredInput, NewStoredSession, NewWorkspaceRegistration, PendingToolExchange, RuntimeStore,
    SessionExecutionEnvironment, StoreErrorKind, StoredAttachmentState, StoredConversationState,
    StoredRunSettlement, StoredSessionLifecycle, StoredWorkspaceLifecycle, UserMessageCommit,
    WorkspaceRemoval,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::{
    DATA_DIRECTORY, DATABASE_FILE, LocalRuntimeStore, SESSIONS_DIRECTORY, StorageEngine,
    append_effect::AppendPurpose,
    body_path, conversation,
    recovery::{AppendRequest, ReplacementPlan},
};

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("session id")
}

fn run_id(value: &str) -> RunId {
    RunId::new(value).expect("run id")
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
        created_at_ms: 1_000,
    }
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

fn tool_results() -> Vec<ToolMessage> {
    tool_exchange()
        .into_iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(message) => Some(message),
            _ => None,
        })
        .collect()
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
                'pending_tool_exchanges', 'body_appends', 'workspaces', 'session_resources'
             )",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(table_count, 8);

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
            input_id: InputId::new("input-files").expect("input id"),
            run_id: run_id("run-files"),
            session_id: session.clone(),
            idempotency_key: None,
            message: message.clone(),
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
        .begin_replacement(session_id("s-rewrite"), replacement.clone(), 3_000)
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
        .begin_replacement(session_id("s-commit"), replacement.clone(), 4_000)
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

fn switch_generation_without_cleanup(connection: &mut Connection, plan: &ReplacementPlan) {
    connection
        .execute(
            "UPDATE sessions
             SET body_generation = ?1, message_count = ?2, updated_at_ms = ?3
             WHERE session_id = ?4 AND body_generation = ?5",
            params![
                i64::try_from(plan.new_generation).expect("new generation"),
                i64::try_from(plan.message_count).expect("message count"),
                plan.updated_at_ms,
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
                input_id: InputId::new(input).expect("input id"),
                run_id: run_id(run),
                session_id: session.clone(),
                idempotency_key: None,
                message: user_message.clone(),
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

    let replacement_user = raw_user_message("user-replacement", "replacement");
    let result = engine
        .rewrite_from_user(ConversationRewrite {
            session_id: session.clone(),
            target_user_message_id: MessageId::new("user-one").expect("message id"),
            conversation: ConversationSnapshot::new(vec![ConversationMessage::User(
                replacement_user.clone(),
            )]),
            input: NewStoredInput {
                input_id: InputId::new("input-replacement").expect("input id"),
                run_id: run_id("run-replacement"),
                session_id: session.clone(),
                idempotency_key: Some(IdempotencyKey::new("rewrite-1").expect("key")),
                message: replacement_user,
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
            input_id: InputId::new("queued-input").expect("input id"),
            run_id: run_id("queued-run"),
            session_id: session.clone(),
            idempotency_key: None,
            message: queued_message,
            accepted_at_ms: 2_004,
        })
        .expect("queued input");
    assert!(matches!(
        engine.set_session_archive(ArchiveChange {
            session_id: session.clone(),
            archived: true,
            changed_at_ms: 2_005,
        }),
        Err(error) if error.kind() == StoreErrorKind::Conflict
    ));
    let recovered = engine.load_runtime().expect("load runtime");
    assert_eq!(
        recovered.sessions[0].lifecycle,
        StoredSessionLifecycle::Active
    );
    assert_eq!(recovered.sessions[0].model_key.as_str(), "other-model");
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
        .join(super::attachment_io::blob_relative_path(&hash));
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
