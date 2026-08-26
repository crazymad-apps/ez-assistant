use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    hint::black_box,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    time::Instant,
};

use agent_core::ExchangeReceipt;
use agent_memory::{MemoryPropertyValue, PinnedMemoryCategory, PinnedMemoryEntry, PinnedMemoryId};
use agent_model::SystemPromptSnapshot;
use agent_types::InternalContextPart;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FileReference,
    FileReferencesPart, FinishReason, MessageId, ModelIdentity, OpaqueProviderState, PartId,
    ProtocolId, ProviderId, ReasoningPart, TextPart, TokenUsage, ToolCall, ToolCallId,
    ToolImageReference, ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultPart,
    ToolResultStatus, TranscriptVisibility, UserMessage, UserMessageOrigin, UserPart,
};
use assistant_protocol::{
    AttachmentId, ChildTaskId, ChildTaskStatus, CompactSessionOutcome, ConversationOwner, GoalId,
    IdempotencyKey, InputId, MessageFeedback, ModelKey, PermissionDiagnosticCode, RunId, RunStatus,
    SessionHistoryCleanupStatus, SessionId, SessionTitleOrigin, TodoItemId, WorkspaceId,
};
use assistant_runtime::{
    ApprovalModeChange, ArchiveChange, ChildTaskStart, ChildToolExecutionStart,
    CompletedChildToolExchange, CompletedToolExchange, ContextReplacement,
    ContextReplacementTarget, ConversationMessageLocationRequest, ConversationRewrite,
    ConversationSearchRequest, ConversationSearchScope, ConversationWindowRequest,
    CrossSessionInputBinding, ForkedAttachmentReference, GoalInputBinding, GoalStop, InputOrigin,
    MessageFeedbackChange, ModelChange, NewAttachmentUpload, NewStoredChildTask, NewStoredInput,
    NewStoredSession, NewWorkspaceRegistration, PendingChildToolExchange, PendingToolExchange,
    PermissionDocument, PermissionEffect, PermissionFileOperation, PermissionFileRevision,
    PermissionFileScope, PermissionFileStore, PersonaMutation, PinnedMemoryCreatedBy,
    PinnedMemoryMutation, QueuePriorityChange, RuntimeStore, SessionDeletion,
    SessionExecutionEnvironment, SessionFork, SessionHistoryClear,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult,
    SessionPinnedChange, SessionProxyChange, SessionRole, SessionSkillCatalog, SessionTitleChange,
    SkillCandidate, SkillDiscovery, SkillDiscoveryStatus, SkillMetadata, SkillName, SkillNameState,
    SkillNameStateChange, SkillSource, StoreErrorKind, StoredAttachmentState,
    StoredChildTaskSettlement, StoredConversationState, StoredGoal, StoredGoalBudget,
    StoredGoalObjective, StoredGoalObjectivePart, StoredGoalPauseReason,
    StoredGoalSettlementEffect, StoredGoalState, StoredRunSettlement, StoredSession,
    StoredSessionLifecycle, StoredTodoItemStatus, StoredWorkPlanItem, StoredWorkspaceLifecycle,
    ToolExecutionStart, UserMessageCommit, VariantChange, WorkPlanClear, WorkPlanMutation,
    WorkspaceRemoval,
};
use assistant_runtime::{SkillActivationOwner, SkillActivationTrigger, StoredSkillActivation};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
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
        reasoning_effort: None,
        system_prompt: SystemPromptSnapshot::new(vec!["stable prompt".to_owned()]),
        skill_catalog: assistant_runtime::SessionSkillCatalog::legacy_unavailable(),
        environment: SessionExecutionEnvironment {
            workspace_id: None,
            working_directory: private_directory.to_string_lossy().into_owned(),
            workspace_private_directory: None,
            session_attachment_directory: session_directory
                .join("attachments")
                .to_string_lossy()
                .into_owned(),
            session_tool_image_directory: session_directory
                .join("tool-images")
                .to_string_lossy()
                .into_owned(),
            session_private_directory: private_directory.to_string_lossy().into_owned(),
        },
        current_variant: assistant_protocol::AgentVariant::Build,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        role: assistant_runtime::SessionRole::Standard,
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
    assert_eq!(document.rules.len(), 16);
    assert!(
        document
            .rules
            .iter()
            .filter(|rule| {
                !matches!(
                    &rule.matcher,
                    assistant_runtime::PermissionMatcher::General(_)
                )
            })
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
    assert!(document.rules.iter().any(|rule| {
        rule.effect == PermissionEffect::Allow
            && matches!(
                &rule.matcher,
                assistant_runtime::PermissionMatcher::General(matcher)
                    if matcher.tool_name == "list_pinned_memories"
            )
    }));
    assert!(document.rules.iter().any(|rule| {
        rule.effect == PermissionEffect::Allow
            && matches!(
                &rule.matcher,
                assistant_runtime::PermissionMatcher::General(matcher)
                    if matcher.tool_name == "recall_memory"
            )
    }));
    for tool_name in ["pin_memory", "update_pinned_memory", "unpin_memory"] {
        assert!(document.rules.iter().any(|rule| {
            rule.effect == PermissionEffect::Ask
                && matches!(
                    &rule.matcher,
                    assistant_runtime::PermissionMatcher::General(matcher)
                        if matcher.tool_name == tool_name
                )
        }));
    }
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

#[test]
fn session_roles_and_proxy_bindings_persist_without_a_controller_unique_constraint() {
    let temporary = TempDir::new().expect("tempdir");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    let mut later = new_session("s-controller-later", &engine.sessions_directory);
    later.role = SessionRole::Controller;
    later.created_at_ms = 20;
    engine
        .create_session(later)
        .expect("create later controller");
    let mut first = new_session("s-controller-first", &engine.sessions_directory);
    first.role = SessionRole::Controller;
    first.created_at_ms = 10;
    engine
        .create_session(first)
        .expect("create first controller");
    engine
        .create_session(new_session("s-standard", &engine.sessions_directory))
        .expect("create standard session");
    engine
        .set_session_proxy(SessionProxyChange {
            target_session_id: session_id("s-standard"),
            controller_session_id: session_id("s-controller-first"),
            enabled: true,
            changed_at_ms: 30,
        })
        .expect("enable proxy");

    drop(engine);
    let mut reopened = StorageEngine::open(temporary.path()).expect("reopen engine");
    let recovered = reopened.load_runtime().expect("recover role and proxy");
    assert_eq!(
        recovered
            .sessions
            .iter()
            .filter(|session| session.role == SessionRole::Controller)
            .count(),
        2
    );
    let standard = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == session_id("s-standard"))
        .expect("standard session");
    assert_eq!(
        standard.proxy,
        Some(assistant_runtime::SessionProxyState {
            controller_session_id: session_id("s-controller-first"),
            changed_at_ms: 30,
        })
    );

    reopened
        .connection
        .execute(
            "UPDATE sessions
             SET proxy_controller_session_id = 's-controller-first', proxy_changed_at_ms = 40
             WHERE session_id = 's-controller-later'",
            [],
        )
        .expect("inject invalid controller proxy");
    assert_eq!(
        reopened
            .load_runtime()
            .expect_err("controller proxy must fail closed")
            .kind(),
        StoreErrorKind::InvalidData
    );
}

#[test]
fn controller_delivery_binding_persists_and_user_takeover_is_atomic() {
    let temporary = TempDir::new().expect("tempdir");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    let mut controller = new_session("s-delivery-controller", &engine.sessions_directory);
    controller.role = SessionRole::Controller;
    engine
        .create_session(controller)
        .expect("create controller");
    engine
        .create_session(new_session("s-delivery-target", &engine.sessions_directory))
        .expect("create target");
    let controller_session_id = session_id("s-delivery-controller");
    let target_session_id = session_id("s-delivery-target");
    let controller_input_id = InputId::new("i-delivery-controller").expect("input id");
    let controller_run_id = run_id("r-delivery-controller");
    let controller_message = raw_user_message("m-delivery-controller", "controller request");
    engine
        .accept_input(NewStoredInput {
            input_id: controller_input_id.clone(),
            run_id: controller_run_id.clone(),
            session_id: controller_session_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: controller_message.clone(),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 10,
        })
        .expect("accept controller input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "commit-delivery-controller".to_owned(),
            input_id: controller_input_id,
            run_id: controller_run_id.clone(),
            session_id: controller_session_id.clone(),
            message: Some(controller_message),
            reasoning_effort: None,
            created_at_ms: 11,
        })
        .expect("start controller run");
    engine
        .set_session_proxy(SessionProxyChange {
            target_session_id: target_session_id.clone(),
            controller_session_id: controller_session_id.clone(),
            enabled: true,
            changed_at_ms: 12,
        })
        .expect("enable proxy");

    let mut delivery_message = raw_user_message("m-delivery-target", "delegated task");
    delivery_message.origin = UserMessageOrigin::Runtime;
    delivery_message.parts.push(UserPart::InternalContext(
        InternalContextPart::new(
            PartId::new("p-delivery-source").expect("part id"),
            "b-delivery-source",
            "controller_delivery",
            Some("controller-delivery:test".to_owned()),
            "delivered by the controller",
        )
        .expect("internal source"),
    ));
    let binding = CrossSessionInputBinding::ControllerDelivery {
        controller_session_id: controller_session_id.clone(),
        controller_run_id: controller_run_id.clone(),
        controller_tool_call_id: assistant_protocol::ToolCallId::new("tc-delivery")
            .expect("tool call id"),
    };
    let accepted = engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("i-delivery-target").expect("input id"),
            run_id: run_id("r-delivery-target"),
            session_id: target_session_id.clone(),
            idempotency_key: Some(IdempotencyKey::new("delivery-key").expect("key")),
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::Runtime,
            goal_binding: None,
            cross_session_binding: Some(binding.clone()),
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: delivery_message,
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 13,
        })
        .expect("accept delivery");
    assert_eq!(accepted.input.cross_session_binding, Some(binding));
    assert!(
        engine
            .load_inputs()
            .expect("load delivery")
            .iter()
            .any(|input| input.input_id == accepted.input.input_id
                && input.cross_session_binding == accepted.input.cross_session_binding)
    );

    engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("i-user-takeover").expect("input id"),
            run_id: run_id("r-user-takeover"),
            session_id: target_session_id.clone(),
            idempotency_key: Some(IdempotencyKey::new("takeover-key").expect("key")),
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: raw_user_message("m-user-takeover", "user request"),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 14,
        })
        .expect("accept takeover");
    let recovered = engine.load_runtime().expect("recover after takeover");
    assert!(
        recovered
            .sessions
            .iter()
            .find(|session| session.session_id == target_session_id)
            .expect("target session")
            .proxy
            .is_none()
    );
    assert!(!recovered.inputs.iter().any(|input| {
        matches!(
            input.cross_session_binding,
            Some(CrossSessionInputBinding::ControllerDelivery { .. })
        )
    }));
    assert!(
        !recovered
            .runs
            .iter()
            .any(|run| run.run_id.as_str() == "r-delivery-target")
    );
}

#[test]
fn proxy_report_is_accepted_atomically_with_source_run_settlement() {
    let temporary = TempDir::new().expect("tempdir");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    let controller_session_id = session_id("s-report-controller");
    let source_session_id = session_id("s-report-source");
    let mut controller = new_session(controller_session_id.as_str(), &engine.sessions_directory);
    controller.role = SessionRole::Controller;
    engine
        .create_session(controller)
        .expect("create controller");
    engine
        .create_session(new_session(
            source_session_id.as_str(),
            &engine.sessions_directory,
        ))
        .expect("create source");
    let source_input_id = InputId::new("i-report-source").expect("input id");
    let source_run_id = run_id("r-report-source");
    let source_message = raw_user_message("m-report-source", "managed work");
    engine
        .accept_input(NewStoredInput {
            input_id: source_input_id.clone(),
            run_id: source_run_id.clone(),
            session_id: source_session_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: source_message.clone(),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 10,
        })
        .expect("accept source input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "commit-report-source".to_owned(),
            input_id: source_input_id,
            run_id: source_run_id.clone(),
            session_id: source_session_id.clone(),
            message: Some(source_message),
            reasoning_effort: None,
            created_at_ms: 11,
        })
        .expect("start source run");
    engine
        .set_session_proxy(SessionProxyChange {
            target_session_id: source_session_id.clone(),
            controller_session_id: controller_session_id.clone(),
            enabled: true,
            changed_at_ms: 12,
        })
        .expect("enable proxy");

    let mut report_message = raw_user_message("m-proxy-report", "source completed");
    report_message.origin = UserMessageOrigin::Runtime;
    report_message.parts.push(UserPart::InternalContext(
        InternalContextPart::new(
            PartId::new("p-proxy-report").expect("part id"),
            "b-proxy-report",
            "proxy_report",
            Some("proxy-report:test".to_owned()),
            "stable source report",
        )
        .expect("report source"),
    ));
    let report_input = NewStoredInput {
        input_id: InputId::new("i-proxy-report").expect("input id"),
        run_id: run_id("r-proxy-report"),
        session_id: controller_session_id.clone(),
        idempotency_key: Some(IdempotencyKey::new("proxy-report-key").expect("key")),
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::Runtime,
        goal_binding: None,
        cross_session_binding: Some(CrossSessionInputBinding::ProxyReport {
            source_session_id: source_session_id.clone(),
            source_run_id: source_run_id.clone(),
            source_goal_id: None,
            source_run_status: RunStatus::Completed,
        }),
        skill_activation: None,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: report_message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 13,
    };
    let result = engine
        .settle_run(StoredRunSettlement {
            operation_id: "settle-report-source".to_owned(),
            run_id: source_run_id.clone(),
            session_id: source_session_id.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("m-report-result", "done")],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: Some(Box::new(report_input.clone())),
            finished_at_ms: 13,
        })
        .expect("settle source and accept report");
    let accepted_report = result.accepted_proxy_report.expect("accepted report");
    assert_eq!(accepted_report.input.input_id, report_input.input_id);
    assert_eq!(accepted_report.run.run_id, report_input.run_id);

    drop(engine);
    let mut reopened = StorageEngine::open(temporary.path()).expect("reopen engine");
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(
        recovered
            .runs
            .iter()
            .find(|run| run.run_id == source_run_id)
            .expect("source run")
            .status,
        RunStatus::Completed
    );
    let recovered_report = recovered
        .inputs
        .iter()
        .find(|input| input.input_id == report_input.input_id)
        .expect("report input");
    assert_eq!(
        recovered_report.state,
        assistant_runtime::StoredInputState::Queued
    );
    assert_eq!(
        recovered_report.cross_session_binding,
        report_input.cross_session_binding
    );
}

#[tokio::test]
async fn skill_name_state_is_durable_and_uses_one_validated_name_key() {
    let temporary = TempDir::new().expect("temporary runtime home");
    let store = LocalRuntimeStore::open(temporary.path(), 4)
        .await
        .expect("open store");
    assert!(
        store
            .list_skill_name_states()
            .await
            .expect("initial states")
            .is_empty()
    );
    let disabled = SkillNameStateChange {
        name: SkillName::parse("review-pr").expect("name"),
        enabled: false,
        updated_at_ms: 10,
    };
    assert_eq!(
        store
            .set_skill_enabled(disabled)
            .await
            .expect("disable skill"),
        SkillNameState {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: false,
            updated_at_ms: 10,
        }
    );
    store.shutdown().await.expect("close store");

    let reopened = LocalRuntimeStore::open(temporary.path(), 4)
        .await
        .expect("reopen store");
    assert_eq!(
        reopened
            .list_skill_name_states()
            .await
            .expect("recovered state"),
        vec![SkillNameState {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: false,
            updated_at_ms: 10,
        }]
    );
    reopened.shutdown().await.expect("close reopened store");
}

#[test]
fn session_skill_catalog_is_recovered_and_forked_without_private_package_copies() {
    let temporary = TempDir::new().expect("tempdir");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    let shared_skill = temporary.path().join("workspace/.agents/skills/review");
    fs::create_dir_all(&shared_skill).expect("shared skill directory");
    fs::write(shared_skill.join("SKILL.md"), b"shared definition").expect("shared definition");
    let shared_resource = shared_skill.join("references/guide.md");
    fs::create_dir_all(shared_resource.parent().expect("resource parent"))
        .expect("shared resource directory");
    fs::write(&shared_resource, b"shared resource v1").expect("shared resource");
    let source_id = session_id("s-skill-source");
    let discovery = SkillDiscovery {
        status: SkillDiscoveryStatus::Available,
        candidates: Vec::new(),
        winners: vec![SkillCandidate {
            name: SkillName::parse("review").expect("name"),
            description: "Review changes".to_owned(),
            source: SkillSource::WorkspaceAgents,
            source_path: shared_skill.to_string_lossy().into_owned(),
            definition_digest: format!("sha256-v1:{}", "1".repeat(64)),
            body: "Review carefully.".to_owned(),
            metadata: SkillMetadata::default(),
            model_invocable: true,
            user_invocable: true,
        }],
        diagnostics: Vec::new(),
    };
    let source_catalog = SessionSkillCatalog::from_discovery(discovery).expect("source catalog");
    let mut source = new_session(source_id.as_str(), &engine.sessions_directory);
    source.system_prompt = source_catalog.augment_system_prompt(source.system_prompt);
    source.skill_catalog = source_catalog.clone();
    let stored = engine.create_session(source).expect("create skill session");
    assert_eq!(stored.skill_catalog, source_catalog);
    assert!(
        !engine
            .sessions_directory
            .join(source_id.as_str())
            .join("private/skills")
            .exists()
    );
    fs::write(&shared_resource, b"shared resource v2").expect("update shared resource");

    let target_id = session_id("s-skill-fork");
    let mut target = new_session(target_id.as_str(), &engine.sessions_directory);
    target.skill_catalog = source_catalog.clone();
    target.system_prompt = stored.system_prompt.clone();
    let forked = engine
        .fork_session(SessionFork {
            source_session_id: source_id,
            source_generation: 1,
            session: target,
            conversation: ConversationSnapshot::new(Vec::new()),
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
        })
        .expect("fork skill session");
    assert_eq!(
        forked.session.skill_catalog.revision,
        source_catalog.revision
    );
    assert_eq!(forked.session.skill_catalog, source_catalog);
    assert!(
        !engine
            .sessions_directory
            .join(target_id.as_str())
            .join("private/skills")
            .exists()
    );
    assert_eq!(
        fs::read(&shared_resource).expect("read current shared resource"),
        b"shared resource v2"
    );
    drop(engine);
    let mut reopened = StorageEngine::open(temporary.path()).expect("reopen engine");
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.sessions.len(), 2);
    assert!(
        recovered
            .sessions
            .iter()
            .all(|session| session.skill_catalog.revision == source_catalog.revision)
    );
    assert!(shared_skill.join("SKILL.md").exists());
}

#[test]
fn user_skill_activation_is_atomic_recoverable_and_forked_as_ledger_fact() {
    let temporary = TempDir::new().expect("tempdir");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    let source_id = session_id("s-skill-activation-source");
    let catalog = SessionSkillCatalog::from_discovery(SkillDiscovery {
        status: SkillDiscoveryStatus::Available,
        candidates: Vec::new(),
        winners: vec![SkillCandidate {
            name: SkillName::parse("review").expect("name"),
            description: "Review changes".to_owned(),
            source: SkillSource::WorkspaceAgents,
            source_path: "/fixture/review".to_owned(),
            definition_digest: format!("sha256-v1:{}", "2".repeat(64)),
            body: "Review carefully.".to_owned(),
            metadata: SkillMetadata::default(),
            model_invocable: true,
            user_invocable: true,
        }],
        diagnostics: Vec::new(),
    })
    .expect("catalog");
    let mut session = new_session(source_id.as_str(), &engine.sessions_directory);
    session.skill_catalog = catalog.clone();
    engine.create_session(session).expect("create source");

    let input_id = InputId::new("input-skill-activation").expect("input id");
    let run_id = run_id("run-skill-activation");
    let mut message = raw_user_message("user-skill-activation", "review this");
    message.parts.push(UserPart::InternalContext(
        InternalContextPart::new(
            PartId::new("part-skill-activation").expect("part id"),
            "boundary-skill-activation".to_owned(),
            "skill_activation".to_owned(),
            Some("skill:review".to_owned()),
            "SKILL_ACTIVATION_V1\nReview carefully.".to_owned(),
        )
        .expect("internal context"),
    ));
    let activation = StoredSkillActivation {
        activation_id: "activation-source".to_owned(),
        session_id: source_id.clone(),
        owner: SkillActivationOwner::Session(source_id.clone()),
        run_id: Some(run_id.clone()),
        input_id: Some(input_id.clone()),
        message_id: message.id.clone(),
        name: SkillName::parse("review").expect("name"),
        catalog_revision: catalog.revision.clone(),
        definition_digest: catalog.definitions[0].definition_digest.clone(),
        trigger: SkillActivationTrigger::User,
        created_at_ms: 10,
    };
    engine
        .accept_input(NewStoredInput {
            input_id: input_id.clone(),
            run_id: run_id.clone(),
            session_id: source_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: Some(activation.clone()),
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: message.clone(),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 10,
        })
        .expect("accept skill input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "commit-skill-activation".to_owned(),
            input_id,
            run_id: run_id.clone(),
            session_id: source_id.clone(),
            message: Some(message),
            reasoning_effort: None,
            created_at_ms: 11,
        })
        .expect("commit user message");
    engine
        .settle_run(StoredRunSettlement {
            message_step: None,
            operation_id: "settle-skill-activation".to_owned(),
            run_id,
            session_id: source_id.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("assistant-skill-activation", "done")],
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 12,
        })
        .expect("settle skill run");
    assert_eq!(
        engine.load_skill_activations().expect("ledger"),
        vec![activation.clone()]
    );

    let target_id = session_id("s-skill-activation-fork");
    let mut target = new_session(target_id.as_str(), &engine.sessions_directory);
    target.skill_catalog = catalog;
    let conversation = engine.load_conversation(&source_id).expect("conversation");
    let fork_activation = StoredSkillActivation {
        activation_id: "activation-fork".to_owned(),
        session_id: target_id.clone(),
        owner: SkillActivationOwner::Session(target_id.clone()),
        run_id: None,
        input_id: None,
        ..activation
    };
    let forked = engine
        .fork_session(SessionFork {
            source_session_id: source_id,
            source_generation: 1,
            session: target,
            conversation,
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: vec![fork_activation.clone()],
            work_plan: None,
            goal: None,
        })
        .expect("fork with activation");
    assert_eq!(forked.skill_activations, vec![fork_activation.clone()]);
    drop(engine);
    let mut reopened = StorageEngine::open(temporary.path()).expect("reopen engine");
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert!(recovered.skill_activations.contains(&fork_activation));
    assert_eq!(recovered.skill_activations.len(), 2);
}

#[test]
fn skill_name_state_transaction_rolls_back_on_sqlite_failure() {
    let temporary = TempDir::new().expect("temporary runtime home");
    let mut engine = StorageEngine::open(temporary.path()).expect("open engine");
    engine
        .set_skill_enabled(SkillNameStateChange {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: false,
            updated_at_ms: 10,
        })
        .expect("initial state");
    engine
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_skill_name_update
             BEFORE UPDATE ON skill_name_states
             BEGIN
                 SELECT RAISE(ABORT, 'fixture failure');
             END;",
        )
        .expect("failure trigger");
    let error = engine
        .set_skill_enabled(SkillNameStateChange {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: true,
            updated_at_ms: 20,
        })
        .expect_err("update must fail");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);
    assert_eq!(
        engine
            .list_skill_name_states()
            .expect("state after rollback"),
        vec![SkillNameState {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: false,
            updated_at_ms: 10,
        }]
    );
}

fn user_message(value: &str, text: &str) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
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

fn assistant_message_with_usage(
    value: &str,
    input_tokens: u64,
    cached_input_tokens: Option<u64>,
) -> ConversationMessage {
    let mut message = assistant_message(value, value);
    let ConversationMessage::Assistant(message) = &mut message else {
        unreachable!("assistant fixture")
    };
    message.usage = Some(TokenUsage {
        input_tokens,
        output_tokens: 10,
        total_tokens: input_tokens + 10,
        cached_input_tokens,
        reasoning_tokens: Some(4),
    });
    ConversationMessage::Assistant(message.clone())
}

#[test]
fn committed_model_requests_update_durable_usage_and_fork_starts_from_zero() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source = session_id("s-usage-source");
    let sessions_directory = engine.sessions_directory.clone();
    seed_session_and_run(&mut engine, source.as_str(), "r-usage");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "usage-one".to_owned(),
            session_id: source.clone(),
            run_id: run_id("r-usage"),
            messages: vec![assistant_message_with_usage("usage-a", 100, Some(60))],
            created_at_ms: 2_000,
        })
        .expect("append first usage");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "usage-two".to_owned(),
            session_id: source.clone(),
            run_id: run_id("r-usage"),
            messages: vec![assistant_message_with_usage("usage-b", 300, Some(240))],
            created_at_ms: 3_000,
        })
        .expect("append second usage");

    let usage = engine.get_session_usage(&source).expect("source usage");
    assert_eq!(usage.request_count, 2);
    assert_eq!(usage.input_tokens, 400);
    assert_eq!(usage.cached_input_tokens, 300);
    assert_eq!(usage.cached_request_count, 2);
    assert_eq!(usage.latest.expect("latest usage").input_tokens, 300);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("request facts"),
        2
    );

    let forked_id = session_id("s-usage-fork");
    let source_conversation = engine
        .load_conversation(&source)
        .expect("source conversation");
    engine
        .fork_session(SessionFork {
            source_session_id: source,
            source_generation: 1,
            session: new_session(forked_id.as_str(), &sessions_directory),
            conversation: source_conversation,
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
        })
        .expect("fork session");
    assert_eq!(
        engine.get_session_usage(&forked_id).expect("fork usage"),
        assistant_runtime::StoredSessionUsage::default()
    );
}

#[test]
fn pending_legacy_usage_is_backfilled_once_from_the_authoritative_conversation() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = session_id("s-usage-backfill");
    seed_session_and_run(&mut engine, session.as_str(), "r-usage-backfill");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "usage-backfill-append".to_owned(),
            session_id: session.clone(),
            run_id: run_id("r-usage-backfill"),
            messages: vec![assistant_message_with_usage(
                "usage-backfill-message",
                200,
                Some(150),
            )],
            created_at_ms: 2_000,
        })
        .expect("append usage");
    engine
        .connection
        .execute("DELETE FROM model_request_records", [])
        .expect("simulate pre-ledger database");
    engine
        .connection
        .execute(
            "UPDATE session_usage SET request_count = 0, input_tokens_sum = 0,
                output_tokens_sum = 0, total_tokens_sum = 0, cached_input_tokens_sum = 0,
                cached_request_count = 0, reasoning_tokens_sum = 0,
                reasoning_request_count = 0, latest_input_tokens = NULL,
                latest_output_tokens = NULL, latest_total_tokens = NULL,
                latest_cached_input_tokens = NULL, latest_reasoning_tokens = NULL,
                backfilled = 0",
            [],
        )
        .expect("mark legacy usage pending");
    drop(engine);

    let mut reopened = open_engine(&root);
    reopened.load_runtime().expect("backfill legacy usage");
    let first = reopened
        .get_session_usage(&session)
        .expect("backfilled usage");
    assert_eq!(first.request_count, 1);
    assert_eq!(first.input_tokens, 200);
    assert_eq!(first.cached_input_tokens, 150);
    drop(reopened);

    let mut reopened = open_engine(&root);
    reopened.load_runtime().expect("second recovery");
    assert_eq!(
        reopened.get_session_usage(&session).expect("stable usage"),
        first
    );
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
                content: ToolResultContent::text("hello".to_owned()),
                metadata: None,
            },
        }),
    ]
}

fn tool_image_exchange(reference: ToolImageReference) -> ConversationSnapshot {
    let call_id = ToolCallId::new("call-image").expect("tool call id");
    ConversationSnapshot::new(vec![
        user_message("user-image", "inspect the image"),
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new("assistant-image").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: call_id.clone(),
                name: ToolName::new("read_image").expect("tool name"),
                arguments: serde_json::json!({"path": "/outside/source.png"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new("tool-image").expect("message id"),
            result: ToolResult {
                call_id,
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![ToolResultPart::image(reference)])
                    .expect("image content"),
                metadata: None,
            },
        }),
    ])
}

fn png_bytes(root: &Path, name: &str, color: [u8; 3]) -> Vec<u8> {
    let path = root.join(name);
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb(color)))
        .save(&path)
        .expect("save png");
    fs::read(path).expect("read png")
}

#[test]
fn fork_copies_tool_images_without_cross_session_links() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source_id = session_id("s-tool-image-source");
    let forked_id = session_id("s-tool-image-fork");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(source_id.as_str(), &sessions_directory))
        .expect("create source session");
    let source_directory = engine
        .session_directory(&source_id)
        .expect("source session")
        .join("tool-images");
    let reference = crate::image::store_tool_image_bytes(
        &source_directory,
        &png_bytes(root.path(), "fork-source.png", [1, 2, 3]),
    )
    .expect("store source image");
    let conversation = tool_image_exchange(reference.clone());

    engine
        .fork_session(SessionFork {
            source_session_id: source_id.clone(),
            source_generation: 1,
            session: new_session(forked_id.as_str(), &sessions_directory),
            conversation,
            attachments: Vec::new(),
            tool_images: vec![reference.clone()],
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
        })
        .expect("fork session with image");

    let source = source_directory.join(reference.relative_path());
    let forked = engine
        .session_directory(&forked_id)
        .expect("forked session")
        .join("tool-images")
        .join(reference.relative_path());
    assert_eq!(
        fs::read(&source).expect("source bytes"),
        fs::read(&forked).expect("fork bytes")
    );
    assert_ne!(
        fs::metadata(&source).expect("source metadata").ino(),
        fs::metadata(&forked).expect("fork metadata").ino()
    );

    let impact = engine
        .inspect_session_deletion(&source_id)
        .expect("source deletion impact");
    engine
        .delete_session(SessionDeletion {
            session_id: source_id,
            operation_id: "delete-tool-image-source".to_owned(),
            expected_impact: impact,
        })
        .expect("delete source session");
    assert!(!source.exists());
    assert_eq!(
        fs::read(&forked).expect("fork image after source deletion"),
        png_bytes(root.path(), "fork-expected.png", [1, 2, 3])
    );
}

#[test]
fn failed_tool_image_fork_rolls_back_target_session_directory() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source_id = session_id("s-tool-image-broken-source");
    let forked_id = session_id("s-tool-image-broken-fork");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(source_id.as_str(), &sessions_directory))
        .expect("create source session");
    let source_directory = engine
        .session_directory(&source_id)
        .expect("source session")
        .join("tool-images");
    let reference = crate::image::store_tool_image_bytes(
        &source_directory,
        &png_bytes(root.path(), "broken-fork-source.png", [4, 5, 6]),
    )
    .expect("store source image");
    let source = source_directory.join(reference.relative_path());
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).expect("make source writable");
    fs::write(&source, b"corrupt").expect("corrupt source image");

    let result = engine.fork_session(SessionFork {
        source_session_id: source_id,
        source_generation: 1,
        session: new_session(forked_id.as_str(), &sessions_directory),
        conversation: tool_image_exchange(reference.clone()),
        attachments: Vec::new(),
        tool_images: vec![reference],
        skill_activations: Vec::new(),
        work_plan: None,
        goal: None,
    });

    assert!(result.is_err());
    assert!(!engine.sessions_directory.join(forked_id.as_str()).exists());
    assert!(engine.inspect_session_deletion(&forked_id).is_err());
}

#[test]
fn startup_tool_image_scan_removes_parts_and_orphans_but_preserves_references() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-tool-image-recovery");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session_id.as_str(), &sessions_directory))
        .expect("create session");
    let session_directory = engine.session_directory(&session_id).expect("session");
    let image_directory = session_directory.join("tool-images");
    let retained = crate::image::store_tool_image_bytes(
        &image_directory,
        &png_bytes(root.path(), "retained.png", [10, 20, 30]),
    )
    .expect("retained image");
    let orphan = crate::image::store_tool_image_bytes(
        &image_directory,
        &png_bytes(root.path(), "orphan.png", [40, 50, 60]),
    )
    .expect("orphan image");
    fs::write(image_directory.join(".crash.part"), b"partial").expect("part file");
    let conversation = tool_image_exchange(retained.clone());
    let payload = conversation::encode_messages(&conversation.messages).expect("conversation");
    fs::write(body_path(&session_directory, 1), payload).expect("write conversation");

    assert!(
        engine
            .recover_tool_images()
            .expect("recover images")
            .is_empty()
    );
    assert!(image_directory.join(retained.relative_path()).is_file());
    assert!(!image_directory.join(orphan.relative_path()).exists());
    assert!(!image_directory.join(".crash.part").exists());
}

#[test]
fn startup_tool_image_scan_is_conservative_when_conversation_is_unreadable() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-tool-image-conservative");
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session_id.as_str(), &sessions_directory))
        .expect("create session");
    let session_directory = engine.session_directory(&session_id).expect("session");
    let image_directory = session_directory.join("tool-images");
    let stable = crate::image::store_tool_image_bytes(
        &image_directory,
        &png_bytes(root.path(), "stable.png", [70, 80, 90]),
    )
    .expect("stable image");
    fs::write(body_path(&session_directory, 1), b"not-jsonl").expect("break conversation");

    let diagnostics = engine.recover_tool_images().expect("recover images");
    assert!(diagnostics.contains(session_id.as_str()));
    assert!(image_directory.join(stable.relative_path()).is_file());
}

#[test]
fn copied_runtime_home_rebases_session_resources_and_preserves_tool_images() {
    let source_root = TempDir::new().expect("source Runtime Home");
    let mut source = open_engine(&source_root);
    let session_id = session_id("s-tool-image-migrated");
    let sessions_directory = source.sessions_directory.clone();
    let stored = source
        .create_session(new_session(session_id.as_str(), &sessions_directory))
        .expect("create source session");
    let source_image_directory = Path::new(&stored.environment.session_tool_image_directory);
    let reference = crate::image::store_tool_image_bytes(
        source_image_directory,
        &png_bytes(source_root.path(), "migrated.png", [12, 34, 56]),
    )
    .expect("store source tool image");
    let conversation = tool_image_exchange(reference.clone());
    fs::write(
        body_path(
            &source
                .session_directory(&session_id)
                .expect("source session directory"),
            1,
        ),
        conversation::encode_messages(&conversation.messages).expect("encode conversation"),
    )
    .expect("write source conversation");
    source
        .connection
        .execute(
            "UPDATE sessions SET message_count = ?1 WHERE session_id = ?2",
            params![
                i64::try_from(conversation.messages.len()).expect("message count"),
                session_id.as_str()
            ],
        )
        .expect("update source message count");
    drop(source);

    let target_root = TempDir::new().expect("target Runtime Home");
    let target_data = target_root.path().join(DATA_DIRECTORY);
    fs::create_dir_all(&target_data).expect("create target data directory");
    fs::copy(
        source_root.path().join(DATA_DIRECTORY).join(DATABASE_FILE),
        target_data.join(DATABASE_FILE),
    )
    .expect("copy Runtime database");
    copy_directory(
        &source_root
            .path()
            .join(DATA_DIRECTORY)
            .join(SESSIONS_DIRECTORY),
        &target_data.join(SESSIONS_DIRECTORY),
    );
    fs::remove_dir_all(source_root.path()).expect("remove old Runtime Home");

    let mut migrated = open_engine(&target_root);
    let recovered = migrated.load_runtime().expect("load migrated Runtime");
    let session = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .expect("migrated session");
    let target_session = target_data
        .join(SESSIONS_DIRECTORY)
        .join(session_id.as_str());
    assert_eq!(
        session.environment.session_private_directory,
        target_session.join("private").to_string_lossy()
    );
    assert_eq!(
        session.environment.session_attachment_directory,
        target_session.join("attachments").to_string_lossy()
    );
    assert_eq!(
        session.environment.session_tool_image_directory,
        target_session.join("tool-images").to_string_lossy()
    );
    assert_eq!(
        migrated
            .load_conversation(&session_id)
            .expect("migrated conversation"),
        conversation
    );
    assert_eq!(
        fs::read(
            target_session
                .join("tool-images")
                .join(reference.relative_path())
        )
        .expect("migrated tool image"),
        png_bytes(target_root.path(), "expected-migrated.png", [12, 34, 56])
    );
    let permission = PermissionDocument::parse(
        &fs::read(target_session.join("private/permissions.json"))
            .expect("migrated permission document"),
    )
    .expect("valid migrated permission document");
    assert!(permission.rules.iter().all(|rule| {
        !matches!(
            &rule.matcher,
            assistant_runtime::PermissionMatcher::File(matcher)
                if matcher.path.contains(source_root.path().to_string_lossy().as_ref())
        )
    }));
    assert_default_session_permissions(session);
    drop(migrated);

    let mut reopened = open_engine(&target_root);
    assert_eq!(
        reopened
            .load_runtime()
            .expect("repeat migrated Runtime recovery")
            .sessions
            .len(),
        1
    );
}

#[test]
fn copied_runtime_home_rebases_workspace_private_resources_only() {
    let source_root = TempDir::new().expect("source Runtime Home");
    let user_workspace = TempDir::new().expect("external user workspace");
    let mut source = open_engine(&source_root);
    let workspace_id = workspace_id("w-runtime-migrated");
    let workspace = source
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id.clone(),
            requested_directory: user_workspace.path().to_string_lossy().into_owned(),
            changed_at_ms: 1_000,
        })
        .expect("register source workspace");
    let expected_user_workspace = workspace.user_directory.clone();
    let session_id = session_id("s-workspace-migrated");
    let mut session = new_session(session_id.as_str(), &source.sessions_directory);
    session.environment.workspace_id = Some(workspace_id.clone());
    session.environment.working_directory = workspace.user_directory.clone();
    session.environment.workspace_private_directory = Some(workspace.agent_directory);
    source
        .create_session(session)
        .expect("create workspace-bound session");
    drop(source);

    let target_root = TempDir::new().expect("target Runtime Home");
    copy_runtime_database_and_sessions(source_root.path(), target_root.path());
    fs::remove_dir_all(source_root.path()).expect("remove old Runtime Home");

    let mut migrated = open_engine(&target_root);
    let recovered = migrated.load_runtime().expect("load migrated Runtime");
    let migrated_workspace = recovered
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)
        .expect("migrated workspace");
    let expected_agent = target_root
        .path()
        .join("data/workspaces")
        .join(workspace_id.as_str())
        .join("agent");
    assert_eq!(
        migrated_workspace.agent_directory,
        expected_agent.to_string_lossy()
    );
    assert_eq!(migrated_workspace.user_directory, expected_user_workspace);
    assert!(expected_agent.join("permissions.json").is_file());

    let migrated_session = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .expect("migrated workspace session");
    assert_eq!(
        migrated_session.environment.working_directory,
        expected_user_workspace
    );
    assert_eq!(
        migrated_session
            .environment
            .workspace_private_directory
            .as_deref(),
        expected_agent.to_str()
    );
    assert!(
        migrated_session
            .environment
            .session_private_directory
            .starts_with(target_root.path().to_string_lossy().as_ref())
    );
}

fn copy_runtime_database_and_sessions(source: &Path, target: &Path) {
    let target_data = target.join(DATA_DIRECTORY);
    fs::create_dir_all(&target_data).expect("create target data directory");
    fs::copy(
        source.join(DATA_DIRECTORY).join(DATABASE_FILE),
        target_data.join(DATABASE_FILE),
    )
    .expect("copy Runtime database");
    copy_directory(
        &source.join(DATA_DIRECTORY).join(SESSIONS_DIRECTORY),
        &target_data.join(SESSIONS_DIRECTORY),
    );
}

fn copy_directory(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create copied directory");
    for entry in fs::read_dir(source).expect("read copied directory") {
        let entry = entry.expect("read copied entry");
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("copied entry type").is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy file");
        }
    }
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
            origin: assistant_runtime::InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            new_goal: None,
            resumed_goal: None,
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
        step: 1,
        receipt: ExchangeReceipt::new(receipt).expect("receipt"),
        session_id: session_id(session),
        run_id: run_id(run),
        assistant,
        created_at_ms: 2_000,
    }
}

fn pending_delegate_exchange(session: &str, run: &str, receipt: &str) -> PendingToolExchange {
    PendingToolExchange {
        step: 1,
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

#[test]
fn persona_and_pinned_memory_use_cas_and_survive_reopen() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    assert_eq!(
        engine
            .load_memory_context()
            .expect("initial memory")
            .persona
            .revision,
        0
    );

    let persona = engine
        .set_persona(PersonaMutation {
            expected_revision: 0,
            enabled: true,
            content: "Prefer concise answers.".to_owned(),
            updated_at_ms: 10,
        })
        .expect("set persona");
    assert_eq!(persona.revision, 1);
    assert_eq!(
        engine
            .set_persona(PersonaMutation {
                expected_revision: 0,
                enabled: false,
                content: String::new(),
                updated_at_ms: 11,
            })
            .expect_err("stale persona mutation")
            .kind(),
        StoreErrorKind::Conflict
    );

    let mut attributes = BTreeMap::new();
    attributes.insert(
        "scope".to_owned(),
        MemoryPropertyValue::String("desktop".to_owned()),
    );
    let original = PinnedMemoryEntry {
        id: PinnedMemoryId::new("memory-one").expect("memory id"),
        category: PinnedMemoryCategory::new("preference").expect("category"),
        content: "Use Chinese by default.".to_owned(),
        attributes,
    };
    let created = engine
        .mutate_pinned_memory(PinnedMemoryMutation::Create {
            entry: original.clone(),
            created_by: PinnedMemoryCreatedBy::AgentTool {
                session_id: session_id("s-memory-author"),
            },
            expected_collection_revision: 0,
            changed_at_ms: 20,
        })
        .expect("create pinned memory");
    assert_eq!(created.collection_revision, 1);
    assert_eq!(created.memory.as_ref().expect("created memory").revision, 1);
    assert_eq!(
        engine
            .mutate_pinned_memory(PinnedMemoryMutation::Create {
                entry: PinnedMemoryEntry {
                    id: PinnedMemoryId::new("memory-two").expect("memory id"),
                    ..original.clone()
                },
                created_by: PinnedMemoryCreatedBy::User,
                expected_collection_revision: 0,
                changed_at_ms: 21,
            })
            .expect_err("stale collection revision")
            .kind(),
        StoreErrorKind::Conflict
    );

    let replacement = PinnedMemoryEntry {
        content: "Use concise Chinese by default.".to_owned(),
        ..original
    };
    let replaced = engine
        .mutate_pinned_memory(PinnedMemoryMutation::Replace {
            entry: replacement.clone(),
            expected_revision: 1,
            changed_at_ms: 30,
        })
        .expect("replace pinned memory");
    assert_eq!(replaced.collection_revision, 2);
    assert_eq!(
        replaced.memory.as_ref().expect("replaced memory").revision,
        2
    );
    assert_eq!(
        engine
            .mutate_pinned_memory(PinnedMemoryMutation::Replace {
                entry: replacement.clone(),
                expected_revision: 1,
                changed_at_ms: 31,
            })
            .expect_err("stale memory revision")
            .kind(),
        StoreErrorKind::Conflict
    );

    drop(engine);
    let mut reopened = open_engine(&root);
    let snapshot = reopened.load_memory_context().expect("reopened memory");
    assert!(snapshot.persona.enabled);
    assert_eq!(snapshot.persona.content, "Prefer concise answers.");
    assert_eq!(snapshot.persona.revision, 1);
    assert_eq!(snapshot.pinned_collection_revision, 2);
    assert_eq!(snapshot.pinned_memories.len(), 1);
    assert_eq!(snapshot.pinned_memories[0].entry, replacement);
    assert_eq!(
        snapshot.pinned_memories[0].created_by,
        PinnedMemoryCreatedBy::AgentTool {
            session_id: session_id("s-memory-author")
        }
    );

    let deleted = reopened
        .mutate_pinned_memory(PinnedMemoryMutation::Delete {
            id: PinnedMemoryId::new("memory-one").expect("memory id"),
            expected_revision: 2,
            changed_at_ms: 40,
        })
        .expect("delete pinned memory");
    assert_eq!(deleted.collection_revision, 3);
    assert!(deleted.memory.is_none());
    drop(reopened);
    assert!(
        open_engine(&root)
            .load_memory_context()
            .expect("memory after delete")
            .pinned_memories
            .is_empty()
    );
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
            origin: assistant_runtime::InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            new_goal: None,
            resumed_goal: None,
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
            reasoning_effort: None,
            created_at_ms: accepted_at_ms + 1,
        })
        .expect("commit fixture user message");
    engine
        .settle_run(StoredRunSettlement {
            message_step: None,
            operation_id: format!("settle-{suffix}"),
            run_id,
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message(&format!("assistant-{suffix}"), suffix)],
            goal_effect: None,
            proxy_report: None,
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
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
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
            media_type: None,
            created_at_ms: 2_000,
        })
        .expect("upload source attachment");
    let source_readable_path = source_attachment.agent_readable_path.clone();
    let conversation = ConversationSnapshot::new(vec![
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
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
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
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
        message_step: None,
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
    let recall = reopened
        .search_conversations(ConversationSearchRequest {
            query: "world".to_owned(),
            scope: ConversationSearchScope::Session {
                session_id: session_id(session),
            },
            limit: 20,
        })
        .expect("search recovered append");
    assert_eq!(recall.hits.len(), 1);
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
                'workspaces', 'session_resources', 'session_work_plans', 'session_goals'
             )",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    assert_eq!(table_count, 11);

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
fn legacy_run_message_ref_without_step_remains_readable_and_uninferred() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-legacy-step", "r-legacy-step");
    engine
        .connection
        .execute(
            "INSERT INTO run_message_refs (run_id, message_id)
             VALUES ('r-legacy-step', 'assistant-legacy-step')",
            [],
        )
        .expect("insert legacy ref without step");

    super::schema::initialize(&mut engine.connection).expect("repeat schema initialization");

    let runs = engine.load_runs().expect("load legacy run");
    assert_eq!(runs[0].message_ids[0].as_str(), "assistant-legacy-step");
    assert!(runs[0].message_steps.is_empty());
    let stored_step: Option<i64> = engine
        .connection
        .query_row(
            "SELECT step FROM run_message_refs WHERE message_id = 'assistant-legacy-step'",
            [],
            |row| row.get(0),
        )
        .expect("read legacy nullable step");
    assert_eq!(stored_step, None);
}

#[test]
fn work_plan_migration_cas_idempotency_restart_and_clear_are_durable() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = session_id("s-work-plan-durable");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    let first_result = engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: 0,
            operation_id: "call-work-plan-1".to_owned(),
            objective: "ship M1".to_owned(),
            items: vec![StoredWorkPlanItem {
                id: TodoItemId::new("todo-work-plan-1").expect("todo id"),
                text: "persist plan".to_owned(),
                status: StoredTodoItemStatus::InProgress,
            }],
            updated_at_ms: 2_000,
        })
        .expect("create work plan");
    assert!(!first_result.cleared);
    let first = first_result.plan;
    assert_eq!(first.revision, 1);

    let duplicate = engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: 0,
            operation_id: "call-work-plan-1".to_owned(),
            objective: "ignored duplicate payload".to_owned(),
            items: Vec::new(),
            updated_at_ms: 3_000,
        })
        .expect("duplicate operation returns first result");
    assert!(!duplicate.cleared);
    assert_eq!(duplicate.plan, first);
    let conflict = engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: 0,
            operation_id: "call-work-plan-2".to_owned(),
            objective: "stale update".to_owned(),
            items: Vec::new(),
            updated_at_ms: 3_000,
        })
        .expect_err("stale revision must conflict");
    assert_eq!(conflict.kind(), StoreErrorKind::Conflict);
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover runtime");
    assert_eq!(recovered.work_plans, vec![first.clone()]);
    reopened
        .clear_work_plan(WorkPlanClear {
            session_id: session.clone(),
            expected_revision: 1,
        })
        .expect("clear plan");
    assert!(
        reopened
            .load_work_plan(&session)
            .expect("load cleared plan")
            .is_none()
    );
    reopened
        .clear_work_plan(WorkPlanClear {
            session_id: session,
            expected_revision: 0,
        })
        .expect("empty clear is idempotent");
}

#[test]
fn completed_or_empty_work_plan_is_atomically_cleared_with_a_durable_operation_receipt() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = session_id("s-work-plan-completed");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    let initial = engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: 0,
            operation_id: "call-work-plan-start".to_owned(),
            objective: "ship completion".to_owned(),
            items: vec![StoredWorkPlanItem {
                id: TodoItemId::new("todo-work-plan-complete").expect("todo id"),
                text: "finish verification".to_owned(),
                status: StoredTodoItemStatus::InProgress,
            }],
            updated_at_ms: 2_000,
        })
        .expect("create work plan");
    let completed_mutation = WorkPlanMutation {
        session_id: session.clone(),
        expected_revision: initial.plan.revision,
        operation_id: "call-work-plan-complete".to_owned(),
        objective: initial.plan.objective.clone(),
        items: vec![StoredWorkPlanItem {
            id: initial.plan.items[0].id.clone(),
            text: initial.plan.items[0].text.clone(),
            status: StoredTodoItemStatus::Completed,
        }],
        updated_at_ms: 3_000,
    };
    let completed = engine
        .mutate_work_plan(completed_mutation.clone())
        .expect("complete work plan");
    assert!(completed.cleared);
    assert_eq!(completed.plan.revision, 2);
    assert!(
        engine
            .load_work_plan(&session)
            .expect("load plan")
            .is_none()
    );
    drop(engine);

    let mut reopened = open_engine(&root);
    assert!(
        reopened
            .load_runtime()
            .expect("recover runtime")
            .work_plans
            .is_empty()
    );
    let duplicate = reopened
        .mutate_work_plan(completed_mutation)
        .expect("replay completed operation");
    assert_eq!(duplicate, completed);
    let replacement = reopened
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: 0,
            operation_id: "call-work-plan-restart".to_owned(),
            objective: "next plan".to_owned(),
            items: vec![StoredWorkPlanItem {
                id: TodoItemId::new("todo-work-plan-next").expect("todo id"),
                text: "start next task".to_owned(),
                status: StoredTodoItemStatus::Pending,
            }],
            updated_at_ms: 4_000,
        })
        .expect("create next work plan");
    assert!(!replacement.cleared);
    assert_eq!(replacement.plan.revision, 1);
    let empty = reopened
        .mutate_work_plan(WorkPlanMutation {
            session_id: session.clone(),
            expected_revision: replacement.plan.revision,
            operation_id: "call-work-plan-empty".to_owned(),
            objective: replacement.plan.objective,
            items: Vec::new(),
            updated_at_ms: 5_000,
        })
        .expect("clear work plan with an empty item list");
    assert!(empty.cleared);
    assert!(
        reopened
            .load_work_plan(&session)
            .expect("load empty-cleared plan")
            .is_none()
    );
}

#[test]
fn legacy_empty_work_plan_is_removed_on_reopen() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = session_id("s-work-plan-legacy-empty");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    engine
        .connection
        .execute(
            "INSERT INTO session_work_plans (
                session_id, revision, objective, items_json, last_operation_id, updated_at_ms
             ) VALUES (?1, 1, 'legacy empty plan', '[]', 'legacy-empty-call', 1)",
            [session.as_str()],
        )
        .expect("insert legacy empty work plan");
    drop(engine);

    let reopened = open_engine(&root);
    assert!(
        reopened
            .load_work_plan(&session)
            .expect("load migrated work plan")
            .is_none()
    );
}

#[test]
fn first_goal_input_goal_and_run_are_atomic_and_idempotent() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-goal-first-input");
    engine
        .create_session(new_session(
            session_id.as_str(),
            &engine.sessions_directory.clone(),
        ))
        .expect("create session");
    let text = TextPart {
        id: PartId::new("goal-objective-text").expect("part id"),
        text: "ship the Goal".to_owned(),
    };
    let objective_part = StoredGoalObjectivePart::Text(text.clone());
    let objective_hash = format!(
        "sha256-v1:{:x}",
        Sha256::digest(
            serde_json::to_vec(&vec![objective_part.clone()]).expect("encode objective")
        )
    );
    let message = UserMessage {
        id: MessageId::new("goal-first-message").expect("message id"),
        origin: UserMessageOrigin::User,
        transcript_visibility: TranscriptVisibility::Visible,
        parts: vec![
            UserPart::Text(text),
            UserPart::Injected(TextPart {
                id: PartId::new("goal-start-context").expect("part id"),
                text: "GOAL_START_INJECTION_V1".to_owned(),
            }),
        ],
    };
    let goal_id = GoalId::new("goal-first").expect("goal id");
    let goal = StoredGoal {
        goal_id: goal_id.clone(),
        session_id: session_id.clone(),
        objective: StoredGoalObjective {
            source_message_id: message.id.clone(),
            payload: vec![objective_part],
            payload_hash: objective_hash,
        },
        state: StoredGoalState::Running,
        pause_reason: None,
        generation: 1,
        turn: 1,
        budget: StoredGoalBudget {
            max_runs: 20,
            max_total_tokens: 500_000,
            max_consecutive_failures: 3,
            used_runs: 0,
            used_total_tokens: 0,
            usage_complete: true,
        },
        consecutive_failures: 0,
        created_at_ms: 2_000,
        updated_at_ms: 2_000,
        completed_at_ms: None,
    };
    let input = NewStoredInput {
        input_id: InputId::new("input-goal-first").expect("input id"),
        run_id: RunId::new("run-goal-first").expect("run id"),
        session_id: session_id.clone(),
        idempotency_key: Some(IdempotencyKey::new("goal-first-key").expect("key")),
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::User,
        goal_binding: Some(GoalInputBinding {
            goal_id: goal_id.clone(),
            generation: 1,
            turn: 1,
        }),
        cross_session_binding: None,
        skill_activation: None,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: message.clone(),
        new_goal: Some(goal.clone()),
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 2_000,
    };
    let accepted = engine
        .accept_input(input.clone())
        .expect("accept Goal input");
    assert!(!accepted.is_duplicate);
    let duplicate = engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("input-goal-duplicate").expect("input id"),
            run_id: RunId::new("run-goal-duplicate").expect("run id"),
            ..input.clone()
        })
        .expect("idempotent duplicate");
    assert!(duplicate.is_duplicate);
    assert_eq!(duplicate.input.input_id, accepted.input.input_id);
    assert_eq!(duplicate.run.run_id, accepted.run.run_id);

    let rejected = engine.accept_input(NewStoredInput {
        input_id: InputId::new("input-second-goal").expect("input id"),
        run_id: RunId::new("run-second-goal").expect("run id"),
        idempotency_key: None,
        ..input
    });
    assert!(rejected.is_err());
    assert_eq!(engine.load_inputs().expect("load inputs").len(), 1);
    assert_eq!(engine.load_runs().expect("load runs").len(), 1);
    assert_eq!(engine.load_all_goals().expect("load goals"), vec![goal]);
    engine
        .connection
        .execute(
            "UPDATE inputs SET origin = 'runtime' WHERE input_id = ?1",
            [accepted.input.input_id.as_str()],
        )
        .expect("corrupt input origin");
    assert!(
        engine.load_inputs().is_err(),
        "an illegal persisted origin/message combination must fail closed"
    );
}

#[test]
fn goal_run_settlement_updates_budget_and_creates_continuation_atomically() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    let session = session_id("s-goal-settlement");
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    let objective_part = StoredGoalObjectivePart::Text(TextPart {
        id: PartId::new("goal-settlement-objective").expect("part id"),
        text: "finish release".to_owned(),
    });
    let objective_hash = format!(
        "sha256-v1:{:x}",
        Sha256::digest(
            serde_json::to_vec(&vec![objective_part.clone()]).expect("encode objective")
        )
    );
    let first_message = UserMessage {
        id: MessageId::new("goal-settlement-user").expect("message id"),
        origin: UserMessageOrigin::User,
        transcript_visibility: TranscriptVisibility::Visible,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("goal-settlement-user-text").expect("part id"),
            text: "finish release".to_owned(),
        })],
    };
    let goal_id = GoalId::new("goal-settlement").expect("goal id");
    let goal = StoredGoal {
        goal_id: goal_id.clone(),
        session_id: session.clone(),
        objective: StoredGoalObjective {
            source_message_id: first_message.id.clone(),
            payload: vec![objective_part],
            payload_hash: objective_hash,
        },
        state: StoredGoalState::Running,
        pause_reason: None,
        generation: 1,
        turn: 1,
        budget: StoredGoalBudget {
            max_runs: 20,
            max_total_tokens: 500_000,
            max_consecutive_failures: 3,
            used_runs: 0,
            used_total_tokens: 0,
            usage_complete: true,
        },
        consecutive_failures: 0,
        created_at_ms: 2_000,
        updated_at_ms: 2_000,
        completed_at_ms: None,
    };
    let first = engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("input-goal-settlement").expect("input id"),
            run_id: RunId::new("run-goal-settlement").expect("run id"),
            session_id: session.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: Some(GoalInputBinding {
                goal_id: goal_id.clone(),
                generation: 1,
                turn: 1,
            }),
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: first_message,
            new_goal: Some(goal.clone()),
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 2_000,
        })
        .expect("accept first Goal input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "start-goal-settlement".to_owned(),
            input_id: first.input.input_id,
            run_id: first.run.run_id.clone(),
            session_id: session.clone(),
            message: first.input.queued_message,
            reasoning_effort: None,
            created_at_ms: 2_100,
        })
        .expect("start first Goal Run");
    let mut updated_goal = goal.clone();
    updated_goal.turn = 2;
    updated_goal.budget.used_runs = 1;
    updated_goal.budget.used_total_tokens = 50;
    updated_goal.updated_at_ms = 3_000;
    let continuation_message = UserMessage {
        id: MessageId::new("goal-continuation-message").expect("message id"),
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        parts: vec![UserPart::Injected(TextPart {
            id: PartId::new("goal-continuation-context").expect("part id"),
            text: "GOAL_CONTINUATION_V1".to_owned(),
        })],
    };
    let next_input = NewStoredInput {
        input_id: InputId::new("input-goal-continuation").expect("input id"),
        run_id: RunId::new("run-goal-continuation").expect("run id"),
        session_id: session.clone(),
        idempotency_key: None,
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::Runtime,
        goal_binding: Some(GoalInputBinding {
            goal_id: goal_id.clone(),
            generation: 1,
            turn: 2,
        }),
        cross_session_binding: None,
        skill_activation: None,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: continuation_message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 3_000,
    };
    let mut invalid_next_input = next_input.clone();
    invalid_next_input
        .goal_binding
        .as_mut()
        .expect("Goal binding")
        .generation = 2;
    assert!(
        engine
            .settle_run(StoredRunSettlement {
                message_step: None,
                operation_id: "reject-invalid-goal-continuation".to_owned(),
                run_id: first.run.run_id.clone(),
                session_id: session.clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message("invalid-goal-answer", "must roll back")],
                goal_effect: Some(StoredGoalSettlementEffect::Continue {
                    expected_goal_id: goal_id.clone(),
                    expected_generation: 1,
                    goal: updated_goal.clone(),
                    next_input: Box::new(invalid_next_input),
                }),
                proxy_report: None,
                finished_at_ms: 3_000,
            })
            .is_err()
    );
    assert_eq!(
        engine.load_runs().expect("Runs after rejected settlement")[0].status,
        RunStatus::Running
    );
    assert_eq!(
        engine.load_all_goals().expect("Goal after rollback"),
        vec![goal.clone()]
    );
    assert_eq!(
        engine.load_inputs().expect("inputs after rollback").len(),
        1
    );
    assert_eq!(
        engine
            .load_conversation(&session)
            .expect("conversation after rollback")
            .messages
            .len(),
        1
    );
    let result = engine
        .settle_run(StoredRunSettlement {
            message_step: None,
            operation_id: "settle-goal-and-continue".to_owned(),
            run_id: first.run.run_id,
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("goal-settlement-answer", "progress")],
            goal_effect: Some(StoredGoalSettlementEffect::Continue {
                expected_goal_id: goal_id,
                expected_generation: 1,
                goal: updated_goal.clone(),
                next_input: Box::new(next_input.clone()),
            }),
            proxy_report: None,
            finished_at_ms: 3_000,
        })
        .expect("settle Goal and create continuation");
    assert_eq!(result.goal, Some(updated_goal.clone()));
    let continuation = result.continuation.expect("continuation result");
    assert_eq!(continuation.input.input_id, next_input.input_id);
    assert_eq!(continuation.run.run_id, next_input.run_id);
    assert_eq!(continuation.input.origin, InputOrigin::Runtime);
    assert_eq!(
        engine.load_all_goals().expect("load Goal"),
        vec![updated_goal.clone()]
    );
    assert_eq!(engine.load_inputs().expect("load inputs").len(), 2);
    assert_eq!(engine.load_runs().expect("load runs").len(), 2);
    let held_message = raw_user_message("goal-resume-user", "use stable channel");
    let held = engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("input-goal-resume").expect("input id"),
            run_id: RunId::new("run-goal-resume").expect("run id"),
            session_id: session.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: held_message.clone(),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 3_500,
        })
        .expect("hold user guidance");

    let mut stopped_goal = updated_goal.clone();
    stopped_goal.state = StoredGoalState::Paused;
    stopped_goal.pause_reason = Some(StoredGoalPauseReason::UserStopped);
    stopped_goal.generation = 2;
    stopped_goal.updated_at_ms = 4_000;
    let stopped = engine
        .stop_goal(GoalStop {
            session_id: session.clone(),
            goal_id: updated_goal.goal_id.clone(),
            expected_generation: 1,
            stopped_goal: stopped_goal.clone(),
        })
        .expect("stop Goal");
    assert_eq!(stopped.removed_input_ids, vec![next_input.input_id]);
    assert!(stopped.cancelling_run_id.is_none());
    assert_eq!(engine.load_inputs().expect("inputs after stop").len(), 2);

    let mut resumed_goal = stopped_goal;
    resumed_goal.state = StoredGoalState::Running;
    resumed_goal.pause_reason = None;
    resumed_goal.generation = 3;
    resumed_goal.turn = 3;
    resumed_goal.updated_at_ms = 5_000;
    let mut resume_message = held_message;
    resume_message.parts.push(UserPart::Injected(TextPart {
        id: PartId::new("goal-resume-context").expect("part id"),
        text: "GOAL_RESUME_INJECTION_V1".to_owned(),
    }));
    let resumed = engine
        .resume_goal_with_held_input(assistant_runtime::GoalHeldInputResume {
            session_id: session.clone(),
            input_id: held.input.input_id,
            expected_goal_id: resumed_goal.goal_id.clone(),
            expected_generation: 2,
            resumed_goal: resumed_goal.clone(),
            message: resume_message,
        })
        .expect("resume Goal with held input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "start-goal-resume".to_owned(),
            input_id: resumed.input.input_id,
            run_id: resumed.run.run_id.clone(),
            session_id: session.clone(),
            message: resumed.input.queued_message,
            reasoning_effort: None,
            created_at_ms: 5_100,
        })
        .expect("start resumed Goal Run");
    let mut completed_goal = resumed_goal;
    completed_goal.state = StoredGoalState::Completed;
    completed_goal.generation = 4;
    completed_goal.budget.used_runs = 2;
    completed_goal.updated_at_ms = 6_000;
    completed_goal.completed_at_ms = Some(6_000);
    let completed_result = engine
        .settle_run(StoredRunSettlement {
            message_step: None,
            operation_id: "complete-resumed-goal".to_owned(),
            run_id: resumed.run.run_id,
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("goal-resume-answer", "done")],
            goal_effect: Some(StoredGoalSettlementEffect::Transition {
                expected_goal_id: completed_goal.goal_id.clone(),
                expected_generation: 3,
                goal: completed_goal.clone(),
                resume_required: false,
            }),
            proxy_report: None,
            finished_at_ms: 6_000,
        })
        .expect("complete resumed Goal");
    assert_eq!(completed_result.goal, Some(completed_goal));
    assert!(
        engine
            .load_all_goals()
            .expect("goals after completion")
            .is_empty()
    );
    assert_eq!(engine.load_inputs().expect("historical inputs").len(), 2);
}

#[test]
fn running_goal_is_durably_paused_once_during_recovery() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    let session = session_id("s-goal-recovery");
    engine
        .create_session(new_session(session.as_str(), &sessions_directory))
        .expect("create session");
    let payload = vec![StoredGoalObjectivePart::Text(TextPart {
        id: PartId::new("goal-objective-part").expect("part id"),
        text: "finish the release".to_owned(),
    })];
    let payload_json = serde_json::to_string(&payload).expect("encode payload");
    let objective_hash = format!(
        "sha256-v1:{:x}",
        Sha256::digest(serde_json::to_vec(&payload).expect("hash payload"))
    );
    engine
        .connection
        .execute(
            "INSERT INTO session_goals (
                goal_id, session_id, objective_message_id, objective_payload_json,
                objective_hash, state, pause_reason_json, generation, turn, max_runs,
                max_total_tokens, max_consecutive_failures, used_runs, used_total_tokens,
                usage_complete, consecutive_failures, created_at_ms, updated_at_ms,
                completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'running', NULL, 1, 1, 20, 500000, 3,
                       1, 100, 1, 0, 1000, 2000, NULL)",
            params![
                GoalId::new("goal-recovery").expect("goal id").as_str(),
                session.as_str(),
                "goal-source-message",
                payload_json,
                objective_hash,
            ],
        )
        .expect("seed running goal");
    engine
        .accept_input(NewStoredInput {
            input_id: InputId::new("input-recovery-continuation").expect("input id"),
            run_id: RunId::new("run-recovery-continuation").expect("run id"),
            session_id: session.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::Runtime,
            goal_binding: Some(GoalInputBinding {
                goal_id: GoalId::new("goal-recovery").expect("goal id"),
                generation: 1,
                turn: 2,
            }),
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: UserMessage {
                id: MessageId::new("recovery-continuation-message").expect("message id"),
                origin: UserMessageOrigin::Runtime,
                transcript_visibility: TranscriptVisibility::Hidden,
                parts: vec![UserPart::Injected(TextPart {
                    id: PartId::new("recovery-continuation-context").expect("part id"),
                    text: "GOAL_CONTINUATION_V1".to_owned(),
                })],
            },
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 2_500,
        })
        .expect("seed accepted continuation");

    let first = engine.load_runtime().expect("first recovery");
    assert_eq!(first.goals.len(), 1);
    assert_eq!(first.goals[0].state, StoredGoalState::Paused);
    assert_eq!(
        first.goals[0].pause_reason,
        Some(StoredGoalPauseReason::RecoveryRequired)
    );
    assert_eq!(first.goals[0].generation, 2);
    assert!(first.inputs.is_empty(), "stale continuation is removed");
    assert!(
        first.runs.is_empty(),
        "continuation Run is removed with Input"
    );

    let second = engine.load_runtime().expect("second recovery");
    assert_eq!(second.goals[0].generation, 2);
    let persisted: (String, String, i64) = engine
        .connection
        .query_row(
            "SELECT state, pause_reason_json, generation FROM session_goals WHERE session_id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("persisted goal recovery state");
    assert_eq!(persisted.0, "paused");
    assert_eq!(persisted.2, 2);
    assert_eq!(
        serde_json::from_str::<StoredGoalPauseReason>(&persisted.1).expect("pause reason"),
        StoredGoalPauseReason::RecoveryRequired
    );
}

#[test]
fn work_plan_fork_is_independent_and_session_delete_cascades() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    let source = session_id("s-work-plan-source");
    let target = session_id("s-work-plan-target");
    engine
        .create_session(new_session(source.as_str(), &sessions_directory))
        .expect("create source");
    let source_plan = engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: source.clone(),
            expected_revision: 0,
            operation_id: "call-source-plan".to_owned(),
            objective: "source objective".to_owned(),
            items: vec![StoredWorkPlanItem {
                id: TodoItemId::new("todo-fork-1").expect("todo id"),
                text: "source item".to_owned(),
                status: StoredTodoItemStatus::Pending,
            }],
            updated_at_ms: 2_000,
        })
        .expect("create source plan")
        .plan;
    let forked = engine
        .fork_session(SessionFork {
            source_session_id: source.clone(),
            source_generation: 1,
            session: new_session(target.as_str(), &sessions_directory),
            conversation: ConversationSnapshot::default(),
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: Some(source_plan.clone()),
            goal: None,
        })
        .expect("fork with plan");
    let target_plan = forked.work_plan.expect("forked plan");
    assert_eq!(target_plan.revision, 1);
    assert_eq!(target_plan.objective, source_plan.objective);
    engine
        .mutate_work_plan(WorkPlanMutation {
            session_id: target.clone(),
            expected_revision: 1,
            operation_id: "call-target-plan".to_owned(),
            objective: "target objective".to_owned(),
            items: target_plan.items,
            updated_at_ms: 3_000,
        })
        .expect("update target plan");
    assert_eq!(
        engine
            .load_work_plan(&source)
            .expect("source plan")
            .expect("source exists")
            .objective,
        "source objective"
    );

    let impact = engine
        .inspect_session_deletion(&target)
        .expect("inspect target deletion");
    engine
        .delete_session(SessionDeletion {
            session_id: target.clone(),
            operation_id: "delete-work-plan-target".to_owned(),
            expected_impact: impact,
        })
        .expect("delete target");
    assert!(
        engine
            .load_work_plan(&target)
            .expect("query deleted plan")
            .is_none()
    );
}

#[test]
fn v0142_storage_migrates_additively_without_losing_existing_business_data() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let source_id = session_id("s-v0142-source");
    let forked_id = session_id("s-v0142-fork");
    let child_session_id = session_id("s-v0142-child");
    let child_id = child_task_id("ct-v0142-child");
    let sessions_directory = engine.sessions_directory.clone();

    let source = engine
        .create_session(new_session(source_id.as_str(), &sessions_directory))
        .expect("create source session");
    commit_completed_turn(&mut engine, &source_id, "v0142-search-marker", 2_000);
    engine
        .rename_session(SessionTitleChange {
            session_id: source_id.clone(),
            title: "v0.14.2 archived session".to_owned(),
            changed_at_ms: 2_100,
        })
        .expect("rename source session");
    engine
        .set_session_pinned(SessionPinnedChange {
            session_id: source_id.clone(),
            is_pinned: true,
            changed_at_ms: 2_101,
        })
        .expect("pin source session");

    let attachment_bytes = b"v0.14.2 attachment payload";
    let attachment_name = "legacy-note.txt";
    let attachment_hash = crate::attachment_hash::digest_bytes(attachment_name, attachment_bytes);
    let staging = engine
        .upload_staging_directory
        .join("v0142-attachment.part");
    fs::write(&staging, attachment_bytes).expect("write attachment staging file");
    let attachment = engine
        .upload_attachment(NewAttachmentUpload {
            attachment_id: AttachmentId::new("a-v0142").expect("attachment id"),
            session_id: source_id.clone(),
            original_name: attachment_name.to_owned(),
            staging_path: staging.to_string_lossy().into_owned(),
            blob_hash: attachment_hash,
            size_bytes: attachment_bytes.len() as u64,
            media_type: None,
            created_at_ms: 2_102,
        })
        .expect("upload legacy attachment");

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
    engine
        .fork_session(SessionFork {
            source_session_id: source_id.clone(),
            source_generation,
            session: new_session(forked_id.as_str(), &sessions_directory),
            conversation: source_conversation.clone(),
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            work_plan: None,
            goal: None,
        })
        .expect("fork legacy session");
    engine
        .set_session_archive(ArchiveChange {
            session_id: source_id.clone(),
            archived: true,
            changed_at_ms: 2_103,
        })
        .expect("archive source session");

    seed_session_and_run(&mut engine, child_session_id.as_str(), "r-v0142-child");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: child_session_id.clone(),
            parent_run_id: run_id("r-v0142-child"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-v0142")
                .expect("tool call id"),
            title: "v0.14.2 child task".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["legacy child prompt".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_104,
        })
        .expect("create legacy child task");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-v0142-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: child_session_id.clone(),
            message: raw_user_message("v0142-child-user", "legacy child marker"),
            started_at_ms: 2_105,
        })
        .expect("start legacy child task");

    let permission_path =
        Path::new(&source.environment.session_private_directory).join("permissions.json");
    let permission_bytes = fs::read(&permission_path).expect("read legacy permission document");

    // v0.14.2 与当前版本共用原有业务表；这里只移除 v0.15 新增对象，构造精确的旧版形态。
    engine
        .connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS conversation_recall_documents_ai;
             DROP TRIGGER IF EXISTS conversation_recall_documents_ad;
             DROP TRIGGER IF EXISTS conversation_recall_documents_au;
             DROP TABLE IF EXISTS conversation_recall_fts;
             DROP TABLE IF EXISTS conversation_recall_documents;
             DROP TABLE IF EXISTS conversation_recall_heads;
             DROP TABLE IF EXISTS pinned_memories;
             DROP TABLE IF EXISTS memory_state;
             DROP TABLE IF EXISTS persona;",
        )
        .expect("downgrade fixture to v0.14.2 shape");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover migrated runtime");
    let source = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == source_id)
        .expect("migrated source session");
    assert_eq!(source.title, "v0.14.2 archived session");
    assert_eq!(source.lifecycle, StoredSessionLifecycle::Archived);
    assert!(source.is_pinned);
    assert_eq!(
        reopened.load_conversation(&source_id).unwrap(),
        source_conversation
    );
    assert_eq!(
        reopened.load_conversation(&forked_id).unwrap(),
        source_conversation
    );
    assert!(
        recovered
            .child_tasks
            .iter()
            .any(|task| task.child_task_id == child_id)
    );
    assert_eq!(
        reopened
            .load_child_conversation(&child_session_id, &child_id)
            .expect("migrated child conversation")
            .messages
            .len(),
        1
    );
    let migrated_attachment = recovered
        .attachments
        .iter()
        .find(|stored| stored.attachment_id == attachment.attachment_id)
        .expect("migrated attachment");
    assert_eq!(
        fs::read(&migrated_attachment.agent_readable_path).expect("read migrated attachment"),
        attachment_bytes
    );
    assert_eq!(
        fs::read(&permission_path).expect("read migrated permission document"),
        permission_bytes
    );

    let memory = reopened
        .load_memory_context()
        .expect("load migrated memory defaults");
    assert_eq!(memory.persona.revision, 0);
    assert!(!memory.persona.enabled);
    assert_eq!(memory.pinned_collection_revision, 0);
    assert!(memory.pinned_memories.is_empty());
    let search = reopened
        .search_conversations(ConversationSearchRequest {
            query: "v0142-search-marker".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("rebuild recall index from migrated conversation");
    assert_eq!(search.hits.len(), 4);
    assert!(!search.partial);
    assert!(search.hits.iter().all(|hit| matches!(
        &hit.owner,
        ConversationOwner::MainSession { session_id }
            if session_id == &source_id || session_id == &forked_id
    )));
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
            step: 1,
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
            step: 1,
            operation_id: "complete-child-tool".to_owned(),
            receipt: ExchangeReceipt::new("child-receipt").expect("receipt"),
            child_task_id: child_id.clone(),
            session_id: session_id("s-child"),
            results: tool_results(),
            activation_message: None,
            skill_activations: Vec::new(),
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
            step: 1,
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
        ToolResultContent::text("runtime restarted; tool execution outcome is unknown".to_owned())
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
                origin: assistant_runtime::InputOrigin::User,
                goal_binding: None,
                cross_session_binding: None,
                skill_activation: None,
                new_goal: None,
                resumed_goal: None,
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
            owner: owner.clone(),
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

    let hidden_id = MessageId::new("runtime-hidden-window").expect("message id");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-runtime-hidden-window".to_owned(),
            session_id: session.clone(),
            run_id: run_id("run-four"),
            messages: vec![ConversationMessage::User(UserMessage {
                id: hidden_id.clone(),
                origin: UserMessageOrigin::Runtime,
                transcript_visibility: TranscriptVisibility::Hidden,
                parts: vec![UserPart::Injected(TextPart {
                    id: PartId::new("runtime-hidden-window-injected").expect("part id"),
                    text: "continue the active goal".to_owned(),
                })],
            })],
            created_at_ms: 1_403,
        })
        .expect("append hidden runtime message");

    let after_hidden = engine
        .load_conversation_window(ConversationWindowRequest {
            owner: owner.clone(),
            generation: 1,
            end: None,
            limit: 2,
        })
        .expect("window after hidden runtime message");
    assert_eq!(
        (after_hidden.start, after_hidden.end, after_hidden.total),
        (6, 8, 8)
    );
    assert_eq!(after_hidden.conversation.messages.len(), 3);
    assert!(!after_hidden.conversation.messages[2].is_transcript_visible());

    let location = engine
        .locate_conversation_message(ConversationMessageLocationRequest {
            owner,
            message_id: hidden_id,
        })
        .expect("locate hidden runtime message")
        .expect("hidden runtime message exists");
    assert_eq!(location.message_ordinal, 8);
    assert_eq!(location.display_ordinal, None);
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
        origin: Default::default(),
        transcript_visibility: Default::default(),
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
            origin: assistant_runtime::InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            new_goal: None,
            resumed_goal: None,
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
            reasoning_effort: None,
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

    let reopened = open_engine(&root);
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
            step: 1,
            operation_id: "append-tool".to_owned(),
            receipt: ExchangeReceipt::new("receipt-tool").expect("receipt"),
            session_id: session_id("s-tool"),
            run_id: run_id("r-tool"),
            results: tool_results(),
            activation_message: None,
            skill_activations: Vec::new(),
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
    let steps = engine
        .connection
        .prepare("SELECT step FROM run_message_refs ORDER BY rowid")
        .expect("prepare ref step query")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("query ref steps")
        .collect::<Result<Vec<_>, _>>()
        .expect("read ref steps");
    assert_eq!(steps, vec![1, 1]);
}

#[test]
fn model_skill_activation_commits_with_tool_results_and_recovers_as_one_fact() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-model-skill", "r-model-skill");
    engine
        .connection
        .execute(
            "UPDATE runs SET status = 'running', started_at_ms = 1_500 \
             WHERE run_id = 'r-model-skill'",
            [],
        )
        .expect("mark run running");
    engine
        .begin_tool_exchange(pending_tool_exchange(
            "s-model-skill",
            "r-model-skill",
            "receipt-model-skill",
        ))
        .expect("begin tool exchange");
    start_tool(
        &mut engine,
        "s-model-skill",
        "r-model-skill",
        "receipt-model-skill",
    );
    let activation_message = UserMessage {
        id: MessageId::new("message-model-skill").expect("message id"),
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        parts: vec![UserPart::InternalContext(
            InternalContextPart::new(
                PartId::new("part-model-skill").expect("part id"),
                "boundary-model-skill".to_owned(),
                "skill_activation".to_owned(),
                Some("skill:review".to_owned()),
                "SKILL_ACTIVATION_V1\ntrigger: model".to_owned(),
            )
            .expect("internal context"),
        )],
    };
    let activation = StoredSkillActivation {
        activation_id: "activation-model-skill".to_owned(),
        session_id: session_id("s-model-skill"),
        owner: SkillActivationOwner::Session(session_id("s-model-skill")),
        run_id: Some(run_id("r-model-skill")),
        input_id: None,
        message_id: activation_message.id.clone(),
        name: SkillName::parse("review").expect("name"),
        catalog_revision: "catalog-revision".to_owned(),
        definition_digest: format!("sha256-v1:{}", "1".repeat(64)),
        trigger: SkillActivationTrigger::Model,
        created_at_ms: 2_500,
    };
    engine
        .complete_tool_exchange(CompletedToolExchange {
            step: 1,
            operation_id: "append-model-skill".to_owned(),
            receipt: ExchangeReceipt::new("receipt-model-skill").expect("receipt"),
            session_id: session_id("s-model-skill"),
            run_id: run_id("r-model-skill"),
            results: tool_results(),
            activation_message: Some(activation_message.clone()),
            skill_activations: vec![activation.clone()],
            completed_at_ms: 2_500,
        })
        .expect("complete model activation exchange");

    let conversation = engine
        .load_conversation(&session_id("s-model-skill"))
        .expect("load conversation");
    assert_eq!(conversation.messages.len(), 3);
    assert_eq!(
        conversation.messages.last(),
        Some(&ConversationMessage::User(activation_message))
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM skill_activations", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count activations"),
        1
    );
    drop(engine);

    let mut reopened = StorageEngine::open(root.path()).expect("reopen engine");
    assert_eq!(
        reopened
            .load_runtime()
            .expect("recover runtime")
            .skill_activations,
        vec![activation]
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
        ToolResultContent::text("runtime restarted before tool execution started".to_owned())
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
            message_step: None,
            operation_id: "settle-pending".to_owned(),
            run_id: run_id("r-pending"),
            session_id: session_id("s-pending"),
            status: assistant_protocol::RunStatus::Failed,
            cancel_requested: false,
            error: None,
            messages: Vec::new(),
            goal_effect: None,
            proxy_report: None,
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
        assistant_protocol::RunStatus::Running
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
        ToolResultContent::text("runtime restarted; tool execution outcome is unknown".to_owned())
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
    assert_eq!(recovered.runs[0].status, RunStatus::Running);
    let conversation = reopened
        .load_conversation(&session_id("s-delegate-complete"))
        .expect("parent conversation");
    let ConversationMessage::Tool(tool) = &conversation.messages[1] else {
        panic!("delegate recovery must append one tool result");
    };
    assert_eq!(tool.result.status, ToolResultStatus::Success);
    let Some(content) = tool.result.content.as_single_json() else {
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
    assert_eq!(recovered.runs[0].status, RunStatus::Running);
    let conversation = reopened
        .load_conversation(&session_id("s-delegate-running"))
        .expect("parent conversation");
    let ConversationMessage::Tool(tool) = &conversation.messages[1] else {
        panic!("delegate recovery must append one tool result");
    };
    let Some(content) = tool.result.content.as_single_json() else {
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
        assistant_protocol::RunStatus::Running
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
fn host_recovery_returns_nonterminal_run_for_runtime_settlement() {
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
        assistant_protocol::RunStatus::Running
    );
    assert!(recovered.runs[0].finished_at_ms.is_none());
}

#[test]
fn startup_finishes_staged_run_start_before_runtime_settlement() {
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
                message_step: None,
                operation_id: "operation-start".to_owned(),
                session_id: session_id("s-start"),
                run_id: run_id("r-start"),
                messages: vec![user_message("user-r-start", "hello")],
                created_at_ms: 2_000,
            },
            AppendPurpose::UserMessage {
                reasoning_effort: None,
            },
        )
        .expect("stage run start");
    drop(engine);

    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("recover staged run start");
    assert_eq!(
        recovered.runs[0].status,
        assistant_protocol::RunStatus::Running
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
            message_step: None,
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
                message_step: None,
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
                goal_effect: None,
                proxy_report: None,
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
        .begin_replacement(session_id("s-rewrite"), replacement.clone(), 2)
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
        .begin_replacement(session_id("s-commit"), replacement.clone(), 2)
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
    let replacement = ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
        id: MessageId::new("message-compact-run").expect("message id"),
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        parts: vec![UserPart::Injected(TextPart {
            id: PartId::new("message-compact-run-injected").expect("part id"),
            text: "continue after summary replacement".to_owned(),
        })],
    })]);

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
    let product_window = engine
        .load_conversation_window(ConversationWindowRequest {
            owner: ConversationOwner::MainSession {
                session_id: session_id("s-compact-run"),
            },
            generation: 2,
            end: None,
            limit: 20,
        })
        .expect("product window after replacement");
    assert_eq!(product_window.total, 0);
    assert!(product_window.conversation.messages.is_empty());
}

#[test]
fn idle_session_compaction_commits_generation_and_receipt_atomically() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    engine
        .create_session(new_session("s-idle-compact", &engine.sessions_directory))
        .expect("create session");
    let operation_id = IdempotencyKey::new("idle-compact-operation").expect("operation id");
    let preparation = SessionHistoryCompactionPreparation {
        operation_id: operation_id.clone(),
        session_id: session_id("s-idle-compact"),
        expected_generation: 1,
        created_at_ms: 2_000,
    };
    assert_eq!(
        engine
            .prepare_session_compaction(preparation.clone())
            .expect("prepare compact"),
        SessionHistoryCompactionPreparationResult::Prepared
    );
    let replacement = ConversationSnapshot::new(vec![user_message(
        "idle-compact-message",
        "retained context",
    )]);
    let committed = engine
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::IdleSession {
                session_id: session_id("s-idle-compact"),
                expected_generation: 1,
                operation_id: operation_id.clone(),
                compacted_message_count: 4,
                retained_message_count: 1,
            },
            conversation: replacement.clone(),
            changed_at_ms: 3_000,
        })
        .expect("commit idle compact");
    assert_eq!(committed.source_generation, 1);
    assert_eq!(committed.result_generation, 2);
    assert_eq!(
        engine
            .load_conversation(&session_id("s-idle-compact"))
            .expect("load compacted conversation"),
        replacement
    );
    assert_eq!(
        engine
            .prepare_session_compaction(preparation)
            .expect("idempotent compact result"),
        SessionHistoryCompactionPreparationResult::Completed(CompactSessionOutcome::Compacted {
            source_generation: 1,
            result_generation: 2,
            compacted_message_count: 4,
            retained_message_count: 1,
        })
    );
    let receipt: (String, i64, i64, i64) = engine
        .connection
        .query_row(
            "SELECT state, result_generation, compacted_message_count, retained_message_count
             FROM session_history_operations WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("compact receipt");
    assert_eq!(receipt, ("completed".to_owned(), 2, 4, 1));
}

#[test]
fn preparing_manual_compaction_recovers_as_interrupted_without_switching_generation() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut engine = open_engine(&root);
    let sessions_directory = engine.sessions_directory.clone();
    engine
        .create_session(new_session("s-compact-recovery", &sessions_directory))
        .expect("create session");
    engine
        .prepare_session_compaction(SessionHistoryCompactionPreparation {
            operation_id: IdempotencyKey::new("compact-recovery").expect("operation id"),
            session_id: session_id("s-compact-recovery"),
            expected_generation: 1,
            created_at_ms: 2_000,
        })
        .expect("prepare compact");
    drop(engine);

    let mut reopened = open_engine(&root);
    reopened.load_runtime().expect("recover runtime");
    let (state, generation): (String, i64) = reopened
        .connection
        .query_row(
            "SELECT operation.state, session.body_generation
             FROM session_history_operations operation
             JOIN sessions session ON session.session_id = operation.session_id
             WHERE operation.operation_id = 'compact-recovery'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("recovered compact receipt");
    assert_eq!(state, "interrupted");
    assert_eq!(generation, 1);
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
                origin: assistant_runtime::InputOrigin::User,
                goal_binding: None,
                cross_session_binding: None,
                skill_activation: None,
                new_goal: None,
                resumed_goal: None,
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
                reasoning_effort: None,
                created_at_ms: at + 1,
            })
            .expect("commit user");
        engine
            .settle_run(StoredRunSettlement {
                message_step: None,
                operation_id: format!("settle-{run}"),
                run_id: run_id(run),
                session_id: session.clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message(assistant, assistant)],
                goal_effect: None,
                proxy_report: None,
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
                origin: assistant_runtime::InputOrigin::User,
                goal_binding: None,
                cross_session_binding: None,
                skill_activation: None,
                new_goal: None,
                resumed_goal: None,
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                input_id: InputId::new("input-replacement").expect("input id"),
                run_id: run_id("run-replacement"),
                session_id: session.clone(),
                idempotency_key: Some(IdempotencyKey::new("rewrite-1").expect("key")),
                message: replacement_user,
                generated_title: None,
                accepted_at_ms: 2_000,
            },
            goal_effect: None,
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
    assert_eq!(recovered.runs[0].status, RunStatus::Accepted);
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
            reasoning_effort: None,
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
            reasoning_effort: None,
            changed_at_ms: 2_003,
        })
        .expect("model change");

    let queued_message = raw_user_message("queued-user", "queued");
    engine
        .accept_input(NewStoredInput {
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: assistant_runtime::InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            new_goal: None,
            resumed_goal: None,
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
            media_type: None,
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
            media_type: None,
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
            media_type: None,
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
            media_type: None,
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
            media_type: None,
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

#[test]
fn recall_index_normalizes_queries_and_only_indexes_visible_message_content() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-visible", "r-recall-visible");
    assert!(engine.recall_index_available);

    let call_id = ToolCallId::new("call-recall-visible").expect("tool call id");
    let messages = vec![
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new("recall-user").expect("message id"),
            parts: vec![
                UserPart::Text(TextPart {
                    id: PartId::new("recall-user-text").expect("part id"),
                    text: "  可见   用户正文 AlphaPath /src/runtime_host.rs ErrorCode::Retry  "
                        .to_owned(),
                }),
                UserPart::FileReferences(FileReferencesPart {
                    id: PartId::new("recall-user-files").expect("part id"),
                    files: vec![FileReference {
                        original_name: "架构图-final.png".to_owned(),
                        readable_path: "/fixture/architecture.png".to_owned(),
                    }],
                }),
            ],
        }),
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new("recall-assistant").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: PartId::new("recall-reasoning").expect("part id"),
                    text: "hidden-reasoning-token".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: PartId::new("recall-assistant-text").expect("part id"),
                    text: "最终可见 AnswerToken".to_owned(),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new("echo_text").expect("tool name"),
                    arguments: serde_json::json!({"secret": "tool-argument-token"}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new("recall-tool-result").expect("message id"),
            result: ToolResult {
                call_id,
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("tool-result-token".to_owned()),
                metadata: None,
            },
        }),
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new("recall-assistant-final").expect("message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider id"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new("recall-assistant-final-text").expect("part id"),
                text: "tool exchange complete".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        }),
        ConversationMessage::User(UserMessage {
            id: MessageId::new("recall-runtime-hidden").expect("message id"),
            origin: UserMessageOrigin::Runtime,
            transcript_visibility: TranscriptVisibility::Hidden,
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("recall-runtime-hidden-text").expect("part id"),
                text: "runtime-hidden-recall-token".to_owned(),
            })],
        }),
    ];
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-visible".to_owned(),
            session_id: session_id("s-recall-visible"),
            run_id: run_id("r-recall-visible"),
            messages,
            created_at_ms: 2_000,
        })
        .expect("append searchable messages");

    for query in [
        "用户正文",
        "alphapath",
        "runtime_host.rs",
        "ErrorCode::Retry",
        "架构图",
        "最终可见",
        "answertoken",
    ] {
        let page = engine
            .search_conversations(ConversationSearchRequest {
                query: query.to_owned(),
                scope: ConversationSearchScope::Session {
                    session_id: session_id("s-recall-visible"),
                },
                limit: 20,
            })
            .expect("search visible content");
        assert!(!page.hits.is_empty(), "expected a hit for {query}");
    }
    for query in [
        "hidden-reasoning-token",
        "tool-argument-token",
        "tool-result-token",
        "architecture.png",
        "runtime-hidden-recall-token",
        "' OR 1=1 --",
    ] {
        let page = engine
            .search_conversations(ConversationSearchRequest {
                query: query.to_owned(),
                scope: ConversationSearchScope::Session {
                    session_id: session_id("s-recall-visible"),
                },
                limit: 20,
            })
            .expect("search filtered content");
        assert!(page.hits.is_empty(), "unexpected hit for {query}");
    }
    let error = engine
        .search_conversations(ConversationSearchRequest {
            query: "中文".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect_err("two-character query must be rejected");
    assert_eq!(error.kind(), StoreErrorKind::InvalidInput);
    let error = engine
        .search_conversations(ConversationSearchRequest {
            query: " \n\t ".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect_err("empty normalized query must be rejected");
    assert_eq!(error.kind(), StoreErrorKind::InvalidInput);
}

#[test]
fn recall_index_applies_session_workspace_and_global_scopes_to_main_and_child_conversations() {
    let root = TempDir::new().expect("runtime home");
    let workspace_directory = TempDir::new().expect("workspace");
    let mut engine = open_engine(&root);
    let workspace = engine
        .register_workspace(NewWorkspaceRegistration {
            workspace_id: workspace_id("w-recall-scope"),
            requested_directory: workspace_directory.path().to_string_lossy().into_owned(),
            changed_at_ms: 100,
        })
        .expect("register workspace");

    for (session_value, in_workspace) in [("s-recall-one", true), ("s-recall-two", false)] {
        let mut session = new_session(session_value, &engine.sessions_directory);
        if in_workspace {
            session.environment.workspace_id = Some(workspace.workspace_id.clone());
            session.environment.working_directory = workspace.user_directory.clone();
            session.environment.workspace_private_directory =
                Some(workspace.agent_directory.clone());
        }
        engine
            .create_session(session)
            .expect("create scoped session");
        commit_completed_turn(
            &mut engine,
            &session_id(session_value),
            &format!("scope-marker-{session_value}"),
            2_000,
        );
    }
    engine
        .set_session_archive(ArchiveChange {
            session_id: session_id("s-recall-one"),
            archived: true,
            changed_at_ms: 2_100,
        })
        .expect("archive workspace recall fixture");

    seed_session_and_run(&mut engine, "s-recall-child", "r-recall-child");
    let child_id = child_task_id("ct-recall-child");
    engine
        .create_child_task(NewStoredChildTask {
            child_task_id: child_id.clone(),
            session_id: session_id("s-recall-child"),
            parent_run_id: run_id("r-recall-child"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-recall-child")
                .expect("tool call id"),
            title: "recall child".to_owned(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_100,
        })
        .expect("create child");
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "start-recall-child".to_owned(),
            child_task_id: child_id.clone(),
            session_id: session_id("s-recall-child"),
            message: raw_user_message("recall-child-user", "child-scope-marker"),
            started_at_ms: 2_200,
        })
        .expect("start child");

    let session_page = engine
        .search_conversations(ConversationSearchRequest {
            query: "child-scope-marker".to_owned(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("s-recall-child"),
            },
            limit: 20,
        })
        .expect("search child in session scope");
    assert_eq!(session_page.hits.len(), 1);
    assert!(matches!(
        &session_page.hits[0].owner,
        ConversationOwner::ChildTask { child_task_id, .. } if child_task_id == &child_id
    ));

    let workspace_page = engine
        .search_conversations(ConversationSearchRequest {
            query: "scope-marker".to_owned(),
            scope: ConversationSearchScope::Workspace {
                workspace_id: workspace.workspace_id,
            },
            limit: 20,
        })
        .expect("search workspace scope");
    assert_eq!(workspace_page.hits.len(), 2);
    assert!(workspace_page.hits.iter().all(|hit| matches!(
        &hit.owner,
        ConversationOwner::MainSession { session_id } if session_id.as_str() == "s-recall-one"
    )));

    let global_page = engine
        .search_conversations(ConversationSearchRequest {
            query: "scope-marker".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("search global scope");
    assert_eq!(global_page.hits.len(), 5);
}

#[test]
fn recall_index_filters_old_generations_and_cascades_session_deletion() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-generation", "r-recall-generation");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-generation".to_owned(),
            session_id: session_id("s-recall-generation"),
            run_id: run_id("r-recall-generation"),
            messages: vec![user_message("recall-old", "legacy-generation-token")],
            created_at_ms: 2_000,
        })
        .expect("append old generation");
    let replacement =
        ConversationSnapshot::new(vec![user_message("recall-new", "current-generation-token")]);
    let plan = engine
        .begin_replacement(session_id("s-recall-generation"), replacement, 2)
        .expect("begin replacement");
    engine
        .commit_replacement(&plan)
        .expect("commit replacement");
    engine
        .connection
        .execute(
            "UPDATE runs
             SET status = 'completed', finished_at_ms = 2001
             WHERE run_id = 'r-recall-generation'",
            [],
        )
        .expect("settle generation fixture run");

    let old = engine
        .search_conversations(ConversationSearchRequest {
            query: "legacy-generation-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("search old generation");
    assert!(old.hits.is_empty());
    let current = engine
        .search_conversations(ConversationSearchRequest {
            query: "current-generation-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("search current generation");
    assert_eq!(current.hits.len(), 1);
    assert_eq!(current.hits[0].generation, plan.new_generation);

    let impact = engine
        .inspect_session_deletion(&session_id("s-recall-generation"))
        .expect("inspect deletion");
    engine
        .delete_session(SessionDeletion {
            session_id: session_id("s-recall-generation"),
            operation_id: "delete-recall-generation".to_owned(),
            expected_impact: impact,
        })
        .expect("delete indexed session");
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_recall_documents",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count recall documents"),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_recall_heads",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .expect("count recall heads"),
        0
    );
}

#[test]
fn recall_index_rebuilds_dirty_owners_in_bounded_batches_after_reopen() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-rebuild", "r-recall-rebuild");
    let messages = (0..300)
        .map(|index| {
            user_message(
                &format!("recall-rebuild-{index}"),
                &format!("batch-rebuild-token-{index}"),
            )
        })
        .collect();
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-rebuild".to_owned(),
            session_id: session_id("s-recall-rebuild"),
            run_id: run_id("r-recall-rebuild"),
            messages,
            created_at_ms: 2_000,
        })
        .expect("append rebuild fixture");
    engine
        .connection
        .execute("DELETE FROM conversation_recall_documents", [])
        .expect("delete derived documents");
    engine
        .connection
        .execute(
            "UPDATE conversation_recall_heads
             SET indexed_message_count = 0, state = 'dirty'",
            [],
        )
        .expect("mark derived index dirty");
    drop(engine);

    let mut reopened = open_engine(&root);
    let untouched: (i64, String) = reopened
        .connection
        .query_row(
            "SELECT indexed_message_count, state FROM conversation_recall_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read dirty head after startup");
    assert_eq!(untouched, (0, "dirty".to_owned()));

    let first = reopened
        .search_conversations(ConversationSearchRequest {
            query: "batch-rebuild-token-0".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("run first rebuild batch");
    assert!(first.partial);
    assert!(first.hits.is_empty());
    let rebuilding: (i64, String) = reopened
        .connection
        .query_row(
            "SELECT indexed_message_count, state FROM conversation_recall_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read rebuilding head");
    assert_eq!(rebuilding, (256, "rebuilding".to_owned()));

    let second = reopened
        .search_conversations(ConversationSearchRequest {
            query: "batch-rebuild-token-0".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("finish rebuild");
    assert!(!second.partial);
    assert_eq!(second.hits.len(), 1);
    let ready: (i64, String) = reopened
        .connection
        .query_row(
            "SELECT indexed_message_count, state FROM conversation_recall_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read ready head");
    assert_eq!(ready, (300, "ready".to_owned()));
}

#[test]
fn recall_index_recovers_interrupted_rebuild_without_scanning_at_startup() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-interrupted", "r-recall-interrupted");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-interrupted".to_owned(),
            session_id: session_id("s-recall-interrupted"),
            run_id: run_id("r-recall-interrupted"),
            messages: vec![user_message(
                "recall-interrupted",
                "interrupted-rebuild-token",
            )],
            created_at_ms: 2_000,
        })
        .expect("append interrupted rebuild fixture");
    engine
        .connection
        .execute(
            "UPDATE conversation_recall_heads
             SET indexed_message_count = 1, state = 'rebuilding'",
            [],
        )
        .expect("stage interrupted rebuild");
    drop(engine);

    let mut reopened = open_engine(&root);
    let head: (i64, String) = reopened
        .connection
        .query_row(
            "SELECT indexed_message_count, state FROM conversation_recall_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read recovered recall head");
    assert_eq!(head, (1, "dirty".to_owned()));
    let page = reopened
        .search_conversations(ConversationSearchRequest {
            query: "interrupted-rebuild-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("rebuild interrupted owner lazily");
    assert!(!page.partial);
    assert_eq!(page.hits.len(), 1);
}

#[test]
fn recall_index_returns_ready_results_when_another_owner_cannot_be_rebuilt() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    for (session, suffix) in [
        ("s-recall-healthy", "partial-recall-token-healthy"),
        ("s-recall-broken", "partial-recall-token-broken"),
    ] {
        let session_id = session_id(session);
        engine
            .create_session(new_session(session, &engine.sessions_directory))
            .expect("create partial recall fixture");
        commit_completed_turn(&mut engine, &session_id, suffix, 2_000);
    }
    let broken_owner = ConversationOwner::MainSession {
        session_id: session_id("s-recall-broken"),
    };
    let broken_body = body_path(
        &engine
            .session_directory(&session_id("s-recall-broken"))
            .expect("broken session directory"),
        1,
    );
    OpenOptions::new()
        .append(true)
        .open(broken_body)
        .expect("open broken conversation")
        .write_all(b"not-json\n")
        .expect("corrupt derived rebuild source");
    engine.mark_recall_owner_dirty_now(&broken_owner, 1);

    let page = engine
        .search_conversations(ConversationSearchRequest {
            query: "partial-recall-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("return partial recall page");
    assert!(page.partial);
    assert_eq!(page.failed_owners, vec![broken_owner.clone()]);
    assert_eq!(page.hits.len(), 2);
    assert!(page.hits.iter().all(|hit| matches!(
        &hit.owner,
        ConversationOwner::MainSession { session_id }
            if session_id.as_str() == "s-recall-healthy"
    )));

    let error = engine
        .search_conversations(ConversationSearchRequest {
            query: "partial-recall-token".to_owned(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("s-recall-broken"),
            },
            limit: 20,
        })
        .expect_err("all unavailable owners must fail explicitly");
    assert_eq!(error.kind(), StoreErrorKind::Unavailable);
}

#[test]
fn missing_fts_is_recreated_as_dirty_without_eager_history_rebuild() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-missing-fts", "r-recall-missing-fts");
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-missing-fts".to_owned(),
            session_id: session_id("s-recall-missing-fts"),
            run_id: run_id("r-recall-missing-fts"),
            messages: vec![user_message("recall-missing-fts", "missing-fts-token")],
            created_at_ms: 2_000,
        })
        .expect("append missing FTS fixture");
    engine
        .connection
        .execute("DROP TABLE conversation_recall_fts", [])
        .expect("drop derived FTS table");
    drop(engine);

    let mut reopened = open_engine(&root);
    assert!(reopened.recall_index_available);
    let derived_count: i64 = reopened
        .connection
        .query_row(
            "SELECT COUNT(*) FROM conversation_recall_documents",
            [],
            |row| row.get(0),
        )
        .expect("count derived documents after startup");
    assert_eq!(derived_count, 0);
    let state: String = reopened
        .connection
        .query_row("SELECT state FROM conversation_recall_heads", [], |row| {
            row.get(0)
        })
        .expect("read missing FTS head");
    assert_eq!(state, "dirty");

    let page = reopened
        .search_conversations(ConversationSearchRequest {
            query: "missing-fts-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 20,
        })
        .expect("lazily rebuild missing FTS");
    assert!(!page.partial);
    assert_eq!(page.hits.len(), 1);
}

#[test]
fn recall_index_failure_never_blocks_authoritative_conversation_commit() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(
        &mut engine,
        "s-recall-index-failure",
        "r-recall-index-failure",
    );
    engine
        .connection
        .execute("DROP TABLE conversation_recall_fts", [])
        .expect("break derived FTS index");

    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-index-failure".to_owned(),
            session_id: session_id("s-recall-index-failure"),
            run_id: run_id("r-recall-index-failure"),
            messages: vec![user_message(
                "recall-index-failure",
                "authority-survives-index-failure",
            )],
            created_at_ms: 2_000,
        })
        .expect("commit conversation despite derived index failure");
    let conversation = engine
        .load_conversation(&session_id("s-recall-index-failure"))
        .expect("load authoritative conversation");
    assert_eq!(conversation.messages.len(), 1);
    let head: (i64, String) = engine
        .connection
        .query_row(
            "SELECT indexed_message_count, state FROM conversation_recall_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read degraded recall head");
    assert_eq!(head, (0, "dirty".to_owned()));
}

#[test]
fn recall_index_enforces_result_limit_and_deterministic_order() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    seed_session_and_run(&mut engine, "s-recall-limit", "r-recall-limit");
    let messages = (0..120)
        .map(|index| {
            user_message(
                &format!("recall-limit-{index:03}"),
                &format!("shared-limit-token item-{index:03}"),
            )
        })
        .collect();
    engine
        .append_messages(AppendRequest {
            message_step: None,
            operation_id: "append-recall-limit".to_owned(),
            session_id: session_id("s-recall-limit"),
            run_id: run_id("r-recall-limit"),
            messages,
            created_at_ms: 2_000,
        })
        .expect("append recall limit fixture");
    let request = ConversationSearchRequest {
        query: "shared-limit-token".to_owned(),
        scope: ConversationSearchScope::Global,
        limit: 500,
    };

    let first = engine
        .search_conversations(request.clone())
        .expect("first bounded recall query");
    let second = engine
        .search_conversations(request)
        .expect("second bounded recall query");
    assert_eq!(first.hits.len(), 100);
    assert_eq!(first.hits, second.hits);
}

#[test]
fn runtime_startup_does_not_scan_committed_conversation_bodies() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = session_id("s-startup-no-body-scan");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create startup fixture");
    commit_completed_turn(&mut engine, &session, "startup-no-body-scan", 2_000);

    let body = body_path(
        &engine
            .session_directory(&session)
            .expect("session directory"),
        1,
    );
    OpenOptions::new()
        .append(true)
        .open(body)
        .expect("open conversation body")
        .write_all(b"not-json\n")
        .expect("append invalid body line");
    engine
        .connection
        .execute("DELETE FROM conversation_recall_documents", [])
        .expect("clear derived recall documents");
    engine
        .connection
        .execute(
            "UPDATE conversation_recall_heads
             SET indexed_message_count = 0, state = 'dirty'",
            [],
        )
        .expect("mark recall owner dirty");
    drop(engine);

    // 已提交正文不属于启动投影；只有用户真正检索该 owner 时才会读取并诊断损坏。
    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("load metadata projection");
    assert_eq!(recovered.sessions.len(), 1);
    assert_eq!(recovered.sessions[0].session_id, session);
    assert_eq!(
        reopened
            .search_conversations(ConversationSearchRequest {
                query: "startup-no-body-scan".to_owned(),
                scope: ConversationSearchScope::Session {
                    session_id: session_id("s-startup-no-body-scan"),
                },
                limit: 20,
            })
            .expect_err("corrupt body must be diagnosed on demand")
            .kind(),
        StoreErrorKind::Unavailable
    );
}

#[test]
#[ignore = "manual v0.15 storage performance baseline"]
fn v015_storage_performance_baseline_reports_reproducible_cases() {
    const SESSION_COUNT: usize = 24;
    const TURNS_PER_SESSION: usize = 12;
    const BATCH_SEARCH_COUNT: usize = 100;

    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    for session_index in 0..SESSION_COUNT {
        let session = session_id(&format!("s-performance-{session_index:02}"));
        engine
            .create_session(new_session(session.as_str(), &engine.sessions_directory))
            .expect("create performance session");
        for turn_index in 0..TURNS_PER_SESSION {
            let suffix =
                format!("performance-{session_index:02}-{turn_index:02}-shared-baseline-token");
            commit_completed_turn(
                &mut engine,
                &session,
                &suffix,
                2_000 + (session_index * TURNS_PER_SESSION + turn_index) as i64 * 10,
            );
        }
    }
    engine
        .connection
        .execute("DELETE FROM conversation_recall_documents", [])
        .expect("clear derived recall documents");
    engine
        .connection
        .execute(
            "UPDATE conversation_recall_heads
             SET indexed_message_count = 0, state = 'dirty'",
            [],
        )
        .expect("mark recall owners dirty");
    drop(engine);

    let startup_started = Instant::now();
    let mut reopened = open_engine(&root);
    let recovered = reopened.load_runtime().expect("load baseline runtime");
    let startup_elapsed = startup_started.elapsed();
    assert_eq!(recovered.sessions.len(), SESSION_COUNT);

    let first_search_started = Instant::now();
    let first_page = reopened
        .search_conversations(ConversationSearchRequest {
            query: "shared-baseline-token".to_owned(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("s-performance-00"),
            },
            limit: 20,
        })
        .expect("first lazy recall search");
    let first_search_elapsed = first_search_started.elapsed();
    assert!(!first_page.partial);
    assert_eq!(first_page.hits.len(), 20);

    let incremental_started = Instant::now();
    commit_completed_turn(
        &mut reopened,
        &session_id("s-performance-00"),
        "incremental-baseline-token",
        50_000,
    );
    let incremental_elapsed = incremental_started.elapsed();
    let incremental_page = reopened
        .search_conversations(ConversationSearchRequest {
            query: "incremental-baseline-token".to_owned(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("s-performance-00"),
            },
            limit: 20,
        })
        .expect("search incrementally indexed turn");
    assert_eq!(incremental_page.hits.len(), 2);

    // 先完成其余 owner 的懒重建，批量检索只衡量稳定索引查询，不混入重建成本。
    for _ in 0..64 {
        let page = reopened
            .search_conversations(ConversationSearchRequest {
                query: "shared-baseline-token".to_owned(),
                scope: ConversationSearchScope::Global,
                limit: 20,
            })
            .expect("warm global recall index");
        if !page.partial {
            break;
        }
    }
    let ready_count: i64 = reopened
        .connection
        .query_row(
            "SELECT COUNT(*) FROM conversation_recall_heads WHERE state = 'ready'",
            [],
            |row| row.get(0),
        )
        .expect("count ready recall owners");
    assert_eq!(ready_count as usize, SESSION_COUNT);

    let batch_started = Instant::now();
    for _ in 0..BATCH_SEARCH_COUNT {
        let page = reopened
            .search_conversations(ConversationSearchRequest {
                query: "shared-baseline-token".to_owned(),
                scope: ConversationSearchScope::Global,
                limit: 20,
            })
            .expect("run stable batch search");
        black_box(page);
    }
    let batch_elapsed = batch_started.elapsed();

    println!(
        "{}",
        serde_json::json!({
            "baseline": "v0.15-storage",
            "sessions": SESSION_COUNT,
            "turns_per_session": TURNS_PER_SESSION,
            "messages": SESSION_COUNT * TURNS_PER_SESSION * 2,
            "batch_search_count": BATCH_SEARCH_COUNT,
            "runtime_startup_us": startup_elapsed.as_micros(),
            "first_session_search_us": first_search_elapsed.as_micros(),
            "incremental_turn_commit_us": incremental_elapsed.as_micros(),
            "batch_search_total_us": batch_elapsed.as_micros(),
            "batch_search_average_us": batch_elapsed.as_micros() / BATCH_SEARCH_COUNT as u128,
        })
    );
}

#[test]
fn recall_index_never_exposes_old_generation_while_rebuild_budget_is_exhausted() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    for index in 0..9 {
        let session = format!("s-recall-stale-{index}");
        let run = format!("r-recall-stale-{index}");
        seed_session_and_run(&mut engine, &session, &run);
        engine
            .append_messages(AppendRequest {
                message_step: None,
                operation_id: format!("append-recall-stale-{index}"),
                session_id: session_id(&session),
                run_id: run_id(&run),
                messages: vec![user_message(
                    &format!("recall-stale-{index}"),
                    "stale-generation-budget-token",
                )],
                created_at_ms: 2_000 + index,
            })
            .expect("append stale generation fixture");
    }
    engine
        .connection
        .execute(
            "UPDATE sessions SET body_generation = 2, message_count = 0",
            [],
        )
        .expect("switch authoritative generations without derived maintenance");

    let page = engine
        .search_conversations(ConversationSearchRequest {
            query: "stale-generation-budget-token".to_owned(),
            scope: ConversationSearchScope::Global,
            limit: 100,
        })
        .expect("search while rebuild owner budget is exhausted");
    assert!(page.partial);
    assert!(page.hits.is_empty());
}

#[test]
fn clear_session_atomically_replaces_history_and_preserves_stable_resources() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let mut controller = new_session("s-clear-controller", &engine.sessions_directory);
    controller.role = SessionRole::Controller;
    engine
        .create_session(controller)
        .expect("create clear controller");
    let target_session_id = session_id("s-clear-target");
    let stored = engine
        .create_session(new_session(
            target_session_id.as_str(),
            &engine.sessions_directory,
        ))
        .expect("create clear target");
    let source_input_id = InputId::new("input-clear-target").expect("input id");
    let source_run_id = run_id("r-clear-target");
    let source_message = raw_user_message("user-clear-target", "clear target");
    engine
        .accept_input(NewStoredInput {
            input_id: source_input_id.clone(),
            run_id: source_run_id.clone(),
            session_id: target_session_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: source_message.clone(),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 2_000,
        })
        .expect("accept clear source input");
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "commit-clear-target".to_owned(),
            input_id: source_input_id,
            run_id: source_run_id.clone(),
            session_id: target_session_id.clone(),
            message: Some(source_message),
            reasoning_effort: None,
            created_at_ms: 2_001,
        })
        .expect("start clear source run");
    engine
        .set_session_proxy(SessionProxyChange {
            target_session_id: target_session_id.clone(),
            controller_session_id: session_id("s-clear-controller"),
            enabled: true,
            changed_at_ms: 2_001,
        })
        .expect("enable clear proxy");
    let report_input_id = InputId::new("i-clear-cross-session-report").expect("input id");
    let mut report_message = raw_user_message("m-clear-cross-session-report", "report");
    report_message.origin = UserMessageOrigin::Runtime;
    report_message.parts.push(UserPart::InternalContext(
        InternalContextPart::new(
            PartId::new("p-clear-cross-session-report").expect("part id"),
            "b-clear-cross-session-report",
            "proxy_report",
            Some("proxy-report:clear-test".to_owned()),
            "stable source report",
        )
        .expect("report source"),
    ));
    let report_binding = CrossSessionInputBinding::ProxyReport {
        source_session_id: target_session_id.clone(),
        source_run_id: source_run_id.clone(),
        source_goal_id: None,
        source_run_status: RunStatus::Completed,
    };
    let report_input = NewStoredInput {
        input_id: report_input_id.clone(),
        run_id: run_id("r-clear-cross-session-report"),
        session_id: session_id("s-clear-controller"),
        idempotency_key: Some(IdempotencyKey::new("clear-cross-session-report").expect("key")),
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::Runtime,
        goal_binding: None,
        cross_session_binding: Some(report_binding.clone()),
        skill_activation: None,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: report_message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 2_002,
    };
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "settle-clear-target".to_owned(),
            run_id: source_run_id,
            session_id: target_session_id.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("assistant-clear-target", "done")],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: Some(Box::new(report_input)),
            finished_at_ms: 2_002,
        })
        .expect("settle source and accept cross-session report");

    let private_marker = Path::new(&stored.environment.session_private_directory).join("keep.txt");
    fs::write(&private_marker, b"private survives").expect("write private marker");
    let attachment_marker =
        Path::new(&stored.environment.session_attachment_directory).join("keep.bin");
    fs::write(&attachment_marker, b"attachment survives").expect("write attachment marker");
    let old_body = body_path(
        &engine
            .session_directory(&target_session_id)
            .expect("session dir"),
        1,
    );
    assert!(old_body.exists());

    let operation_id = IdempotencyKey::new("clear-operation-1").expect("operation id");
    let result = engine
        .clear_session_history(SessionHistoryClear {
            operation_id: operation_id.clone(),
            session_id: target_session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["rebuilt prompt".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment: stored.environment.clone(),
            expected_role: SessionRole::Standard,
            changed_at_ms: 4_000,
        })
        .expect("clear target history");

    assert_eq!(result.source_generation, 1);
    assert_eq!(result.result_generation, 2);
    assert_eq!(
        result.cleanup_status,
        SessionHistoryCleanupStatus::Completed
    );
    assert_eq!(result.session.title, "New Session");
    assert_eq!(result.session.message_count, 0);
    assert_eq!(result.session.body_generation, 2);
    assert_eq!(result.session.proxy, None);
    assert_eq!(
        result.session.system_prompt,
        SystemPromptSnapshot::new(vec!["rebuilt prompt".to_owned()])
    );
    assert!(
        engine
            .load_conversation(&target_session_id)
            .expect("empty body")
            .messages
            .is_empty()
    );
    assert!(!old_body.exists());
    assert!(body_path(&engine.session_directory(&target_session_id).unwrap(), 2).exists());
    assert_eq!(
        fs::read(private_marker).expect("private marker"),
        b"private survives"
    );
    assert_eq!(
        fs::read(attachment_marker).expect("attachment marker"),
        b"attachment survives"
    );
    assert!(
        engine
            .load_inputs()
            .expect("load inputs")
            .iter()
            .all(|input| input.session_id != target_session_id)
    );
    assert!(
        engine
            .load_inputs()
            .expect("load cross-session report")
            .iter()
            .any(|input| input.input_id == report_input_id
                && input.cross_session_binding == Some(report_binding.clone()))
    );

    let replay = engine
        .clear_session_history(SessionHistoryClear {
            operation_id,
            session_id: target_session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["ignored retry prompt".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment: stored.environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 5_000,
        })
        .expect("replay clear operation");
    assert_eq!(replay, result);
}

#[test]
fn clear_session_keeps_user_title_and_rejects_a_busy_snapshot() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-clear-busy");
    let stored = engine
        .create_session(new_session(session_id.as_str(), &engine.sessions_directory))
        .expect("create busy clear target");
    engine
        .rename_session(SessionTitleChange {
            session_id: session_id.clone(),
            title: "Keep my title".to_owned(),
            changed_at_ms: 1_100,
        })
        .expect("rename clear target");
    let queued = raw_user_message("m-clear-busy", "queued");
    let queued_input_id = InputId::new("i-clear-busy").expect("input id");
    engine
        .accept_input(NewStoredInput {
            input_id: queued_input_id.clone(),
            run_id: run_id("r-clear-busy"),
            session_id: session_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session_binding: None,
            skill_activation: None,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            message: queued,
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 2_000,
        })
        .expect("accept queued clear input");
    let error = engine
        .clear_session_history(SessionHistoryClear {
            operation_id: IdempotencyKey::new("clear-busy-operation").expect("operation id"),
            session_id: session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["new prompt".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment: stored.environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 3_000,
        })
        .expect_err("busy clear must fail");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);
    let session = engine
        .load_sessions()
        .expect("load unchanged session")
        .into_iter()
        .find(|session| session.session_id == session_id)
        .expect("busy session");
    assert_eq!(session.body_generation, 1);
    assert_eq!(session.title, "Keep my title");

    engine
        .cancel_queued_input(&session_id, &queued_input_id)
        .expect("remove busy input");
    let environment = engine
        .load_session_environment(&session_id)
        .expect("stable clear environment");
    let cleared = engine
        .clear_session_history(SessionHistoryClear {
            operation_id: IdempotencyKey::new("clear-user-title-operation").expect("operation id"),
            session_id: session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["new prompt".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 4_000,
        })
        .expect("clear user titled session");
    assert_eq!(cleared.session.title, "Keep my title");
    assert_eq!(cleared.session.title_origin, SessionTitleOrigin::User);
}

#[test]
fn clear_cleanup_pending_keeps_new_history_authoritative_and_recovers() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-clear-recovery");
    let stored = engine
        .create_session(new_session(session_id.as_str(), &engine.sessions_directory))
        .expect("create clear recovery target");
    commit_completed_turn(&mut engine, &session_id, "clear-recovery", 2_000);
    let unexpected =
        Path::new(&stored.environment.session_tool_image_directory).join("unexpected.txt");
    fs::write(&unexpected, b"blocks exact cleanup").expect("write unexpected image entry");

    let result = engine
        .clear_session_history(SessionHistoryClear {
            operation_id: IdempotencyKey::new("clear-recovery-operation").expect("operation id"),
            session_id: session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["recovered prompt".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment: stored.environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 3_000,
        })
        .expect("clear with pending cleanup");
    assert_eq!(result.cleanup_status, SessionHistoryCleanupStatus::Pending);
    assert_eq!(engine.session_generation(&session_id).unwrap(), 2);
    assert!(
        engine
            .load_conversation(&session_id)
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(body_path(&engine.session_directory(&session_id).unwrap(), 1).exists());
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT state FROM session_history_operations
                 WHERE operation_id = 'clear-recovery-operation'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("pending receipt"),
        "cleanup_pending"
    );

    drop(engine);
    let mut engine = open_engine(&root);
    let recovered = engine.load_runtime().expect("load pending clear runtime");
    let recovered_session = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .expect("recovered clear session");
    assert_eq!(recovered_session.body_generation, 2);
    assert_eq!(
        recovered_session.conversation_state,
        StoredConversationState::Available
    );

    fs::remove_file(unexpected).expect("remove cleanup blocker");
    engine
        .recover_session_history_operations()
        .expect("recover pending cleanup");
    assert!(!body_path(&engine.session_directory(&session_id).unwrap(), 1).exists());
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT state FROM session_history_operations
                 WHERE operation_id = 'clear-recovery-operation'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("completed receipt"),
        "completed"
    );
}

#[test]
fn interrupted_clear_preparation_removes_only_the_unpublished_generation() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-clear-preparing-recovery");
    engine
        .create_session(new_session(session_id.as_str(), &engine.sessions_directory))
        .expect("create preparing clear fixture");
    commit_completed_turn(&mut engine, &session_id, "clear-preparing-recovery", 2_000);
    engine
        .connection
        .execute(
            "INSERT INTO session_history_operations (
                operation_id, session_id, kind, state, source_generation,
                result_generation, created_at_ms, finished_at_ms
             ) VALUES ('clear-preparing-crash', ?1, 'clear', 'preparing', 1, 2, 3_000, NULL)",
            [session_id.as_str()],
        )
        .expect("insert preparing receipt");
    let session_directory = engine.session_directory(&session_id).expect("session dir");
    super::create_new_private_file(&body_path(&session_directory, 2))
        .expect("create unpublished body");
    drop(engine);

    let mut engine = open_engine(&root);
    let recovered = engine.load_runtime().expect("recover preparing clear");
    let session = recovered
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .expect("recovered session");
    assert_eq!(session.body_generation, 1);
    assert_eq!(session.message_count, 2);
    assert!(!body_path(&session_directory, 2).exists());
    assert_eq!(
        engine
            .load_conversation(&session_id)
            .unwrap()
            .messages
            .len(),
        2
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT state FROM session_history_operations
                 WHERE operation_id = 'clear-preparing-crash'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("interrupted receipt"),
        "interrupted"
    );
}

#[test]
fn clear_switch_transaction_failure_keeps_old_history_and_removes_new_file() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session_id = session_id("s-clear-switch-failure");
    let stored = engine
        .create_session(new_session(session_id.as_str(), &engine.sessions_directory))
        .expect("create switch failure fixture");
    commit_completed_turn(&mut engine, &session_id, "clear-switch-failure", 2_000);
    engine
        .connection
        .execute_batch(
            "CREATE TEMP TRIGGER fail_clear_switch
             BEFORE UPDATE OF body_generation ON sessions
             BEGIN SELECT RAISE(FAIL, 'injected clear switch failure'); END;",
        )
        .expect("install clear switch failure");

    let error = engine
        .clear_session_history(SessionHistoryClear {
            operation_id: IdempotencyKey::new("clear-switch-failure").expect("operation id"),
            session_id: session_id.clone(),
            expected_generation: 1,
            system_prompt: SystemPromptSnapshot::new(vec!["must not commit".to_owned()]),
            skill_catalog: SessionSkillCatalog::legacy_unavailable(),
            environment: stored.environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 3_000,
        })
        .expect_err("clear switch transaction must fail");
    assert_eq!(error.kind(), StoreErrorKind::Conflict);
    assert_eq!(engine.session_generation(&session_id).unwrap(), 1);
    assert_eq!(
        engine
            .load_conversation(&session_id)
            .unwrap()
            .messages
            .len(),
        2
    );
    assert!(!body_path(&engine.session_directory(&session_id).unwrap(), 2).exists());
    assert!(
        engine
            .load_inputs()
            .expect("old input remains")
            .iter()
            .any(|input| input.session_id == session_id)
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT state FROM session_history_operations
                 WHERE operation_id = 'clear-switch-failure'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("interrupted transaction receipt"),
        "interrupted"
    );
}

#[tokio::test]
async fn recall_queries_share_the_bounded_store_worker() {
    let root = TempDir::new().expect("runtime home");
    let mut engine = open_engine(&root);
    let session = session_id("s-recall-worker");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .expect("create worker recall fixture");
    commit_completed_turn(&mut engine, &session, "worker-recall-token", 2_000);
    drop(engine);

    let store = LocalRuntimeStore::open(root.path(), 2)
        .await
        .expect("open recall store worker");
    let request = ConversationSearchRequest {
        query: "worker-recall-token".to_owned(),
        scope: ConversationSearchScope::Global,
        limit: 20,
    };
    let (first, second) = tokio::join!(
        store.search_conversations(request.clone()),
        store.search_conversations(request),
    );
    assert_eq!(first.expect("first worker search").hits.len(), 2);
    assert_eq!(second.expect("second worker search").hits.len(), 2);
    store.shutdown().await.expect("shutdown recall worker");
}
