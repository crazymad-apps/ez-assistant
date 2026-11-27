use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_testkit::{ModelScript, ScriptedModelService, message_events};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileOperation, Tool, ToolContext, ToolError,
    ToolExecuteFuture, ToolExecutionMode, ToolRegistry, ToolResolution, ToolSetSnapshot,
};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, ProviderId, ToolCall,
    ToolCallId, ToolName,
};
use assistant_protocol::{
    AgentVariant, ApprovalDecision, CreateSessionRequest, DecideApprovalRequest,
    GetPermissionDocumentRequest, ListPendingApprovalsRequest, PermissionDiagnosticCode,
    PermissionDocumentDraft, PermissionDocumentRevision, PermissionDocumentScope,
    PermissionFileMatcher, PermissionFileOperationDefinition, PermissionFileStatus,
    PermissionPathMatch, PermissionRuleDefinition, PermissionRuleEffect, PermissionRuleMatcher,
    PermissionScope, RegisterWorkspaceRequest, ReloadPermissionsRequest,
    ReplacePermissionDocumentRequest, RunStatus, RuntimeEvent, SubmitInputRequest,
};

use super::*;
use crate::{
    PermissionDocument, PermissionFileLoad, PermissionFileRevision, PermissionFileScope,
    PermissionFileStore, PermissionStoreFuture, StoreError, StoreErrorKind,
    storage::VolatileRuntimeStore,
};

pub(super) struct MutablePermissionStore {
    files: Mutex<BTreeMap<PermissionFileScope, Vec<u8>>>,
    replace_behavior: Mutex<ReplaceBehavior>,
}

#[derive(Default)]
enum ReplaceBehavior {
    #[default]
    Fail,
    ConflictOnce,
    Succeed,
}

impl Default for MutablePermissionStore {
    fn default() -> Self {
        Self {
            files: Mutex::new(BTreeMap::new()),
            replace_behavior: Mutex::new(ReplaceBehavior::Fail),
        }
    }
}

impl MutablePermissionStore {
    pub(super) fn writable() -> Self {
        Self {
            files: Mutex::new(BTreeMap::new()),
            replace_behavior: Mutex::new(ReplaceBehavior::Succeed),
        }
    }

    pub(super) fn conflict_once() -> Self {
        Self {
            files: Mutex::new(BTreeMap::new()),
            replace_behavior: Mutex::new(ReplaceBehavior::ConflictOnce),
        }
    }

    pub(super) fn put(&self, scope: PermissionFileScope, content: impl Into<Vec<u8>>) {
        self.files
            .lock()
            .expect("permission files")
            .insert(scope, content.into());
    }
}

impl PermissionFileStore for MutablePermissionStore {
    fn load_permission_file(
        &self,
        scope: &PermissionFileScope,
    ) -> PermissionStoreFuture<'_, PermissionFileLoad> {
        let result = self
            .files
            .lock()
            .map_err(|_| {
                StoreError::new(
                    StoreErrorKind::Unavailable,
                    "test permission store is unavailable",
                )
            })
            .map(|files| match files.get(scope) {
                Some(content) => PermissionFileLoad {
                    revision: revision_for(content),
                    content: Some(content.clone()),
                    diagnostics: Vec::new(),
                },
                None => PermissionFileLoad {
                    content: None,
                    revision: PermissionFileRevision::Missing,
                    diagnostics: Vec::new(),
                },
            });
        Box::pin(async move { result })
    }

    fn replace_permission_file(
        &self,
        scope: &PermissionFileScope,
        _expected_revision: &PermissionFileRevision,
        content: Vec<u8>,
    ) -> PermissionStoreFuture<'_, PermissionFileRevision> {
        let result = self
            .replace_behavior
            .lock()
            .map_err(|_| StoreError::new(StoreErrorKind::Unavailable, "replace lock failed"))
            .and_then(|mut behavior| match *behavior {
                ReplaceBehavior::Fail => Err(StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "test permission store does not support replacement",
                )),
                ReplaceBehavior::ConflictOnce => {
                    *behavior = ReplaceBehavior::Succeed;
                    Err(StoreError::new(
                        StoreErrorKind::Conflict,
                        "scripted permission conflict",
                    ))
                }
                ReplaceBehavior::Succeed => {
                    let mut files = self.files.lock().map_err(|_| {
                        StoreError::new(StoreErrorKind::Unavailable, "file lock failed")
                    })?;
                    let current_revision = files
                        .get(scope)
                        .map_or(PermissionFileRevision::Missing, |current| {
                            revision_for(current)
                        });
                    if &current_revision != _expected_revision {
                        return Err(StoreError::new(
                            StoreErrorKind::Conflict,
                            "scripted permission revision conflict",
                        ));
                    }
                    let revision = revision_for(&content);
                    files.insert(scope.clone(), content);
                    Ok(revision)
                }
            });
        Box::pin(async move { result })
    }
}

fn revision_for(content: &[u8]) -> PermissionFileRevision {
    PermissionFileRevision::Content(format!("test-{}", content.len()))
}

async fn runtime_with_permission_store(
    permission_store: Arc<MutablePermissionStore>,
) -> AssistantRuntime {
    runtime_with_permission_components(permission_store, empty_model(), ToolSetSnapshot::default())
        .await
}

pub(super) async fn runtime_with_permission_components(
    permission_store: Arc<MutablePermissionStore>,
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::open(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(tools),
        Arc::new(TestChildWorkspaceFactory::default()),
        Arc::new(VolatileRuntimeStore::default()),
        permission_store,
    )
    .await
    .expect("open runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;
    runtime
}

fn global_document(rule_id: &str) -> Vec<u8> {
    format!(
        r#"{{
  "schema_version": 1,
  "rules": [
    {{
      "id": "{rule_id}",
      "effect": "allow",
      "variants": ["build"],
      "matcher": {{
        "type": "general",
        "tool_name": "fixture"
      }}
    }}
  ]
}}
"#
    )
    .into_bytes()
}

fn file_calls_step(message_id: &str, paths: &[&str]) -> ModelScript {
    ModelScript::Events(message_events(&AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId::new(format!("{message_id}-call-{index}"))
                        .expect("tool call id"),
                    name: ToolName::new("read_file").expect("tool name"),
                    arguments: serde_json::json!({ "path": path }),
                })
            })
            .collect(),
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }))
}

fn file_tools() -> ToolSetSnapshot {
    let mut registry = ToolRegistry::new();
    registry
        .register(ParallelReadTool)
        .expect("register read file tool");
    registry.snapshot()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ParallelReadInput {
    path: String,
}

#[derive(serde::Serialize)]
struct ParallelReadOutput {
    content: &'static str,
}

struct ParallelReadTool;

impl Tool for ParallelReadTool {
    type Input = ParallelReadInput;
    type ResolvedInput = AbsolutePath;
    type Output = ParallelReadOutput;

    fn name(&self) -> ToolName {
        ToolName::new("read_file").expect("tool name")
    }

    fn description(&self) -> String {
        "Read a file in a permission queue test".to_owned()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::ParallelEligible
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = AbsolutePath::new(
            std::path::Path::new(&test_absolute_path("/workspace")).join(input.path),
        )
        .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        Ok(ToolResolution::with_facts(
            path.clone(),
            FileAuthorizationFacts {
                operation: FileOperation::Read,
                path: path.clone(),
            },
            serde_json::json!({ "path": path }),
        ))
    }

    fn execute<'a>(
        &'a self,
        _input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(std::future::ready(Ok(ParallelReadOutput {
            content: "content",
        })))
    }
}

async fn wait_for_pending_count(
    runtime: &AssistantRuntime,
    session_id: &assistant_protocol::SessionId,
    count: usize,
) -> Vec<assistant_protocol::ApprovalSnapshot> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let approvals = runtime
                .list_pending_approvals(ListPendingApprovalsRequest {
                    session_id: session_id.clone(),
                })
                .await
                .expect("list pending approvals")
                .approvals;
            if approvals.len() == count {
                return approvals;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending approval count reached")
}

#[tokio::test]
async fn reload_replaces_the_whole_session_cohort_and_publishes_one_fact() {
    let source = Arc::new(MutablePermissionStore::default());
    source.put(PermissionFileScope::Global, global_document("global-v1"));
    let runtime = runtime_with_permission_store(source.clone()).await;
    let workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "permission-reload".to_owned(),
            primary_directory: Some("/workspace/permission-reload".to_owned()),
            additional_directories: Vec::new(),
        })
        .await
        .expect("register workspace")
        .workspace;
    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(workspace.workspace_id.clone()),
        })
        .await
        .expect("create session")
        .session;
    source.put(
        PermissionFileScope::Workspace(workspace.workspace_id),
        PermissionDocument::empty()
            .render()
            .expect("workspace JSON"),
    );
    let mut events = runtime.subscribe_events();

    let result = runtime
        .reload_permissions(ReloadPermissionsRequest {
            session_id: session.session_id.clone(),
        })
        .await
        .expect("reload permissions");

    assert!(result.applied);
    assert_eq!(
        result.files,
        vec![
            assistant_protocol::PermissionFileSummary {
                scope: PermissionScope::Global,
                status: PermissionFileStatus::Ready,
            },
            assistant_protocol::PermissionFileSummary {
                scope: PermissionScope::Workspace,
                status: PermissionFileStatus::Ready,
            },
            assistant_protocol::PermissionFileSummary {
                scope: PermissionScope::Session,
                status: PermissionFileStatus::Empty,
            },
        ]
    );
    assert!(matches!(
        events.recv().await.expect("reload event"),
        RuntimeEvent::PermissionReloaded { session_id, .. } if session_id == session.session_id
    ));
}

#[tokio::test]
async fn invalid_member_keeps_the_previous_cohort_intact() {
    let source = Arc::new(MutablePermissionStore::default());
    source.put(PermissionFileScope::Global, global_document("global-v1"));
    let runtime = runtime_with_permission_store(source.clone()).await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    runtime
        .reload_permissions(ReloadPermissionsRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("initial reload");

    source.put(PermissionFileScope::Global, global_document("global-v2"));
    source.put(
        PermissionFileScope::Session(session_id.clone()),
        br#"{"schema_version":1,"rules":["invalid"]}"#.to_vec(),
    );
    let result = runtime
        .reload_permissions(ReloadPermissionsRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("invalid reload is a diagnostic result");

    assert!(!result.applied);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.scope == PermissionScope::Session
            && matches!(
                diagnostic.code,
                PermissionDiagnosticCode::InvalidDocument | PermissionDiagnosticCode::InvalidRule
            )
    }));
    let global = runtime
        .permission_coordinator
        .registry()
        .snapshot(&PermissionFileScope::Global)
        .expect("registry")
        .expect("global snapshot");
    assert_eq!(
        global.document.as_ref().expect("valid global").rules[0].id,
        "global-v1"
    );
    let session = runtime
        .permission_coordinator
        .registry()
        .snapshot(&PermissionFileScope::Session(session_id))
        .expect("registry")
        .expect("session snapshot");
    assert_eq!(session.status, PermissionFileStatus::Empty);
}

#[tokio::test]
async fn permission_documents_are_projected_replaced_and_guarded_by_revision() {
    let source = Arc::new(MutablePermissionStore::writable());
    let runtime = runtime_with_permission_store(source).await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let scope = PermissionDocumentScope::Session {
        session_id: session_id.clone(),
    };

    let initial = runtime
        .get_permission_document(GetPermissionDocumentRequest {
            scope: scope.clone(),
        })
        .await
        .expect("read empty session permission document")
        .document;
    assert_eq!(initial.revision, PermissionDocumentRevision::Missing);
    assert_eq!(initial.status, PermissionFileStatus::Empty);
    assert!(initial.editable);

    let draft = PermissionDocumentDraft {
        schema_version: 1,
        rules: vec![PermissionRuleDefinition {
            id: "allow-private-read".to_owned(),
            effect: PermissionRuleEffect::Allow,
            variants: vec![AgentVariant::Build],
            matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                operation: PermissionFileOperationDefinition::Read,
                path: test_absolute_path("/private/session"),
                path_match: PermissionPathMatch::Exact,
            }),
        }],
    };
    let replaced = runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: scope.clone(),
            expected_revision: initial.revision.clone(),
            document: draft.clone(),
        })
        .await
        .expect("replace session permission document")
        .document;
    assert_eq!(replaced.status, PermissionFileStatus::Ready);
    assert_eq!(replaced.rules, draft.rules);

    let stale = runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope,
            expected_revision: PermissionDocumentRevision::Missing,
            document: draft,
        })
        .await
        .expect_err("stale permission revision must not overwrite content");
    assert!(matches!(stale, RuntimeError::PermissionFileConflict));
}

#[tokio::test]
async fn permission_document_replacement_rejects_global_and_invalid_candidates() {
    let source = Arc::new(MutablePermissionStore::writable());
    let runtime = runtime_with_permission_store(source).await;
    let global = runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Global,
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: Vec::new(),
            },
        })
        .await
        .expect_err("global permission document is read-only");
    assert!(matches!(global, RuntimeError::InvalidRequest { .. }));

    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let invalid = runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Session {
                session_id: session_id.clone(),
            },
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![PermissionRuleDefinition {
                    id: "relative-file-rule".to_owned(),
                    effect: PermissionRuleEffect::Allow,
                    variants: vec![AgentVariant::Build],
                    matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                        operation: PermissionFileOperationDefinition::Read,
                        path: "relative/path".to_owned(),
                        path_match: PermissionPathMatch::Exact,
                    }),
                }],
            },
        })
        .await
        .expect_err("relative path candidate is invalid");
    assert!(matches!(invalid, RuntimeError::PermissionFileInvalid));

    let unchanged = runtime
        .get_permission_document(GetPermissionDocumentRequest {
            scope: PermissionDocumentScope::Session { session_id },
        })
        .await
        .expect("invalid candidate did not overwrite the document")
        .document;
    assert_eq!(unchanged.revision, PermissionDocumentRevision::Missing);
    assert_eq!(unchanged.status, PermissionFileStatus::Empty);
}

#[tokio::test]
async fn saving_an_exact_file_rule_drains_matching_approvals_from_the_queue_head() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("assistant-file-tools", &["repeated.txt", "repeated.txt"]),
            ModelScript::Events(message_events(&assistant_text("assistant-final", "done"))),
        ],
    ));
    let runtime = runtime_with_permission_components(
        Arc::new(MutablePermissionStore::writable()),
        model,
        file_tools(),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "read twice".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    assert_eq!(
        wait_for_pending_count(&runtime, &session_id, 2).await.len(),
        2
    );

    runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Session {
                session_id: session_id.clone(),
            },
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![PermissionRuleDefinition {
                    id: "allow-repeated-read".to_owned(),
                    effect: PermissionRuleEffect::Allow,
                    variants: vec![AgentVariant::Build],
                    matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                        operation: PermissionFileOperationDefinition::Read,
                        path: test_absolute_path("/workspace/repeated.txt"),
                        path_match: PermissionPathMatch::Exact,
                    }),
                }],
            },
        })
        .await
        .expect("save exact permission rule");

    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .await
            .expect("list approvals")
            .approvals
            .is_empty()
    );
}

#[tokio::test]
async fn saving_a_recursive_file_rule_does_not_release_an_existing_approval() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("assistant-file-tool", &["repeated.txt"]),
            ModelScript::Events(message_events(&assistant_text("assistant-final", "done"))),
        ],
    ));
    let runtime = runtime_with_permission_components(
        Arc::new(MutablePermissionStore::writable()),
        model,
        file_tools(),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "read once".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_count(&runtime, &session_id, 1)
        .await
        .pop()
        .expect("pending approval");

    runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Session {
                session_id: session_id.clone(),
            },
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![PermissionRuleDefinition {
                    id: "allow-workspace-reads".to_owned(),
                    effect: PermissionRuleEffect::Allow,
                    variants: vec![AgentVariant::Build],
                    matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                        operation: PermissionFileOperationDefinition::Read,
                        path: test_absolute_path("/workspace"),
                        path_match: PermissionPathMatch::Recursive,
                    }),
                }],
            },
        })
        .await
        .expect("save recursive permission rule");

    let approvals = runtime
        .list_pending_approvals(ListPendingApprovalsRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("list approvals after save")
        .approvals;
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].approval_id, pending.approval_id);

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny retained approval");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn global_and_controller_permission_scopes_are_isolated_in_both_directions() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("ctrl-global-only", &["global-only.txt"]),
            ModelScript::Events(message_events(&assistant_text(
                "ctrl-global-done",
                "denied",
            ))),
            file_calls_step("std-global-only", &["global-only.txt"]),
            ModelScript::Events(message_events(&assistant_text(
                "std-global-done",
                "allowed",
            ))),
            file_calls_step("ctrl-own-rule", &["controller-only.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-own-done", "allowed"))),
            file_calls_step("std-ctrl-only", &["controller-only.txt"]),
            ModelScript::Events(message_events(&assistant_text("std-ctrl-done", "denied"))),
        ],
    ));
    let permission_store = Arc::new(MutablePermissionStore::writable());
    let global_doc = PermissionDocument {
        schema_version: 1,
        rules: vec![crate::PermissionRule {
            id: "allow-global-only".to_owned(),
            effect: crate::PermissionEffect::Allow,
            variants: vec![AgentVariant::Build],
            matcher: crate::PermissionMatcher::File(crate::FilePermissionMatcher {
                operation: crate::PermissionFileOperation::Read,
                path: test_absolute_path("/workspace/global-only.txt"),
                path_match: crate::PathMatch::Exact,
                excluded_paths: Vec::new(),
            }),
        }],
    }
    .render()
    .expect("render global doc");
    permission_store.put(PermissionFileScope::Global, global_doc);
    let runtime = runtime_with_permission_components(permission_store, model, file_tools()).await;
    runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Controller,
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![PermissionRuleDefinition {
                    id: "allow-controller-only".to_owned(),
                    effect: PermissionRuleEffect::Allow,
                    variants: vec![AgentVariant::Build],
                    matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                        operation: PermissionFileOperationDefinition::Read,
                        path: test_absolute_path("/workspace/controller-only.txt"),
                        path_match: PermissionPathMatch::Exact,
                    }),
                }],
            },
        })
        .await
        .expect("save controller rule");

    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("controller")
        .session
        .session_id;
    let standard = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("standard")
        .session
        .session_id;

    let ctrl_global_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "read global file from controller".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("ctrl global input")
        .run;
    let pending = wait_for_pending_count(&runtime, &controller, 1)
        .await
        .pop()
        .expect("global allow must not skip approval on controller");
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: controller.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny");
    let term = wait_for_terminal(&runtime, &controller, &ctrl_global_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Failed
    );

    let std_global_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: standard.clone(),
            message: "read global file from standard".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("std global input")
        .run;
    let term = wait_for_terminal(&runtime, &standard, &std_global_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Completed
    );

    let ctrl_own_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "read controller file from controller".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("ctrl own input")
        .run;
    let term = wait_for_terminal(&runtime, &controller, &ctrl_own_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Completed
    );

    let std_ctrl_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: standard.clone(),
            message: "read controller file from standard".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("std ctrl input")
        .run;
    let pending = wait_for_pending_count(&runtime, &standard, 1)
        .await
        .pop()
        .expect("controller allow must not skip approval on standard");
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: standard.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny");
    let term = wait_for_terminal(&runtime, &standard, &std_ctrl_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Failed
    );
}

#[tokio::test]
async fn controller_grants_consume_fifo_respect_expiry_revocation_deny_and_isolate_standard() {
    use assistant_protocol::{
        CreatePermissionGrantRequest, ListPermissionGrantsRequest, RevokePermissionGrantRequest,
    };

    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("std-blocked", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text(
                "std-blocked-done",
                "denied",
            ))),
            file_calls_step("ctrl-use-1", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-use-1-done", "ok1"))),
            file_calls_step("ctrl-use-2", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-use-2-done", "ok2"))),
            file_calls_step("ctrl-use-3", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-use-3-done", "ok3"))),
            file_calls_step("ctrl-exhausted", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text(
                "ctrl-exhausted-done",
                "denied",
            ))),
            file_calls_step("ctrl-deny-override", &["a.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-deny-done", "denied"))),
        ],
    ));
    let store = Arc::new(VolatileRuntimeStore::default());
    let runtime = AssistantRuntime::open(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(file_tools()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store.clone(),
        Arc::new(MutablePermissionStore::writable()),
    )
    .await
    .expect("open runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;

    let read_matcher = PermissionRuleMatcher::File(PermissionFileMatcher {
        operation: PermissionFileOperationDefinition::Read,
        path: test_absolute_path("/workspace"),
        path_match: PermissionPathMatch::Recursive,
    });
    // Pre-seed an expired grant to verify expiry is ignored without sleeping.
    crate::RuntimeStore::create_permission_grant(
        store.as_ref(),
        crate::StoredPermissionGrant {
            grant_id: "expired-grant".to_owned(),
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: crate::permission::matcher_from_protocol(read_matcher.clone()),
            max_uses: 1,
            remaining_uses: 1,
            expires_at_ms: Some(1),
            note: "expired".to_owned(),
            created_at_ms: 1,
            consumed_at_ms: None,
        },
    )
    .await
    .expect("seed expired grant");

    let grant_1 = runtime
        .create_permission_grant(CreatePermissionGrantRequest {
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: read_matcher.clone(),
            max_uses: 2,
            ttl_seconds: Some(3_600),
            note: "first grant".to_owned(),
        })
        .await
        .expect("create grant 1")
        .grant;
    // Ensure a strictly later millisecond so FIFO ordering does not tie-break on random grant_id.
    while i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after unix epoch")
            .as_millis(),
    )
    .expect("timestamp fits i64")
        <= grant_1.created_at_ms
    {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let grant_2 = runtime
        .create_permission_grant(CreatePermissionGrantRequest {
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: read_matcher.clone(),
            max_uses: 1,
            ttl_seconds: None,
            note: "second grant".to_owned(),
        })
        .await
        .expect("create grant 2")
        .grant;
    let revoked_grant = runtime
        .create_permission_grant(CreatePermissionGrantRequest {
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: read_matcher.clone(),
            max_uses: 1,
            ttl_seconds: None,
            note: "revoked grant".to_owned(),
        })
        .await
        .expect("create revoked grant")
        .grant;
    assert!(
        runtime
            .revoke_permission_grant(RevokePermissionGrantRequest {
                grant_id: revoked_grant.grant_id.clone(),
            })
            .await
            .expect("revoke grant")
            .revoked
    );

    let expired_only_excluded = runtime
        .list_permission_grants(ListPermissionGrantsRequest {
            include_exhausted: false,
        })
        .await
        .expect("list active grants only")
        .grants;
    assert_eq!(
        expired_only_excluded
            .iter()
            .map(|grant| grant.grant_id.as_str())
            .collect::<Vec<_>>(),
        vec![grant_1.grant_id.as_str(), grant_2.grant_id.as_str()]
    );
    assert!(
        !expired_only_excluded
            .iter()
            .any(|grant| grant.grant_id == "expired-grant")
    );
    assert!(
        runtime
            .list_permission_grants(ListPermissionGrantsRequest {
                include_exhausted: true,
            })
            .await
            .expect("include_exhausted keeps expired grant")
            .grants
            .iter()
            .any(|grant| grant.grant_id == "expired-grant")
    );

    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("controller")
        .session
        .session_id;
    let standard = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("standard")
        .session
        .session_id;

    let std_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: standard.clone(),
            message: "standard must not use controller grant".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("std input")
        .run;
    let pending = wait_for_pending_count(&runtime, &standard, 1)
        .await
        .pop()
        .expect("standard needs approval despite controller grants");
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: standard.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny");
    assert_eq!(
        wait_for_terminal(&runtime, &standard, &std_run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    // Uses 1 & 2 come from grant_1 in FIFO order; use 3 comes from grant_2; use 4 exhausts grants.
    for (step_index, msg) in ["use 1", "use 2", "use 3"].iter().enumerate() {
        let run = runtime
            .submit_started_input(SubmitInputRequest {
                delivery: Default::default(),
                mode: assistant_protocol::SubmitInputMode::Normal,
                session_id: controller.clone(),
                message: (*msg).to_owned(),
                variant: AgentVariant::Build,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await
            .expect("controller run")
            .run;
        let term = wait_for_terminal(&runtime, &controller, &run.run_id).await;
        assert_eq!(
            term.tools[0].status,
            assistant_protocol::ToolActivityStatus::Completed,
            "{msg} should be authorized by grant"
        );
        if step_index == 0 {
            let after_first = runtime
                .list_permission_grants(ListPermissionGrantsRequest {
                    include_exhausted: false,
                })
                .await
                .expect("after first use")
                .grants;
            let partial = after_first
                .iter()
                .find(|grant| grant.grant_id == grant_1.grant_id)
                .expect("grant_1 remains active after first use");
            assert_eq!(partial.remaining_uses, 1);
            assert_eq!(partial.consumed_at_ms, None);
        }
    }

    let exhausted_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "after exhaustion".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("exhausted input")
        .run;
    let pending = wait_for_pending_count(&runtime, &controller, 1)
        .await
        .pop()
        .expect("exhausted and expired grants must require approval");
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: controller.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny exhausted");
    wait_for_terminal(&runtime, &controller, &exhausted_run.run_id).await;

    let remaining = runtime
        .list_permission_grants(ListPermissionGrantsRequest {
            include_exhausted: true,
        })
        .await
        .expect("list all grants")
        .grants;
    let grant_1_after = remaining
        .iter()
        .find(|g| g.grant_id == grant_1.grant_id)
        .expect("grant 1 present");
    assert_eq!(grant_1_after.remaining_uses, 0);
    assert!(grant_1_after.consumed_at_ms.is_some());
    assert_eq!(
        remaining
            .iter()
            .find(|g| g.grant_id == grant_2.grant_id)
            .expect("grant 2 present")
            .remaining_uses,
        0
    );

    // Explicit Deny rule on Controller takes precedence over a newly created matching grant without consuming it.
    let grant_3 = runtime
        .create_permission_grant(CreatePermissionGrantRequest {
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: read_matcher,
            max_uses: 1,
            ttl_seconds: None,
            note: "must not override deny".to_owned(),
        })
        .await
        .expect("create grant 3")
        .grant;
    runtime
        .replace_permission_document(ReplacePermissionDocumentRequest {
            scope: PermissionDocumentScope::Controller,
            expected_revision: PermissionDocumentRevision::Missing,
            document: PermissionDocumentDraft {
                schema_version: 1,
                rules: vec![PermissionRuleDefinition {
                    id: "explicit-deny-read".to_owned(),
                    effect: PermissionRuleEffect::Deny,
                    variants: vec![AgentVariant::Build],
                    matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                        operation: PermissionFileOperationDefinition::Read,
                        path: test_absolute_path("/workspace"),
                        path_match: PermissionPathMatch::Recursive,
                    }),
                }],
            },
        })
        .await
        .expect("save controller deny");

    let denied_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "deny takes precedence".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("deny input")
        .run;
    let term = wait_for_terminal(&runtime, &controller, &denied_run.run_id).await;
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Failed
    );
    assert_eq!(
        runtime
            .list_permission_grants(ListPermissionGrantsRequest {
                include_exhausted: false,
            })
            .await
            .expect("active grants")
            .grants
            .into_iter()
            .find(|g| g.grant_id == grant_3.grant_id)
            .expect("grant 3 must remain unconsumed")
            .remaining_uses,
        1
    );
}

#[tokio::test]
async fn device_originated_controller_turn_fails_fast_without_grant_and_consumes_when_granted() {
    use assistant_protocol::{CreatePermissionGrantRequest, ListPermissionGrantsRequest};

    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("device-no-grant", &["secret.txt"]),
            ModelScript::Events(message_events(&assistant_text(
                "device-denied",
                "no permission",
            ))),
            file_calls_step("device-with-grant", &["secret.txt"]),
            ModelScript::Events(message_events(&assistant_text("device-allowed", "read ok"))),
        ],
    ));
    let runtime = runtime_with_permission_components(
        Arc::new(MutablePermissionStore::writable()),
        model,
        file_tools(),
    )
    .await;

    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("controller")
        .session
        .session_id;
    let device = runtime
        .register_paired_device(crate::NewPairedDevice {
            device_id: assistant_protocol::DeviceId::new("device-perm").expect("device id"),
            display_name: "智能终端".to_owned(),
            public_key: crate::DevicePublicKey::new([9; 32]),
            paired_at_ms: 1_000,
        })
        .await
        .expect("register device");

    let make_device_input = |client_input_id: &str, msg: &str| crate::SubmitSessionInputRequest {
        input: SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: msg.to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        },
        source: crate::InputChannelSource::Device(crate::DeviceInputSource {
            device_id: device.device_id.clone(),
            client_input_id: client_input_id.to_owned(),
            modality: crate::InputModality::Text,
            requested_output: crate::OutputPreference::Text,
        }),
    };

    let fast_fail_run = runtime
        .submit_started_session_input(make_device_input("c-1", "read without grant"))
        .await
        .expect("device input 1")
        .run;
    let term = wait_for_terminal(&runtime, &controller, &fast_fail_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Failed
    );
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: controller.clone(),
            })
            .await
            .expect("approvals")
            .approvals
            .is_empty()
    );

    let grant = runtime
        .create_permission_grant(CreatePermissionGrantRequest {
            scope: PermissionScope::Controller,
            variants: vec![AgentVariant::Build],
            matcher: PermissionRuleMatcher::File(PermissionFileMatcher {
                operation: PermissionFileOperationDefinition::Read,
                path: test_absolute_path("/workspace"),
                path_match: PermissionPathMatch::Recursive,
            }),
            max_uses: 1,
            ttl_seconds: Some(3_600),
            note: "device voice one-shot".to_owned(),
        })
        .await
        .expect("create grant")
        .grant;

    let authorized_run = runtime
        .submit_started_session_input(make_device_input("c-2", "read with grant"))
        .await
        .expect("device input 2")
        .run;
    let term = wait_for_terminal(&runtime, &controller, &authorized_run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Completed
    );
    assert!(
        !runtime
            .list_permission_grants(ListPermissionGrantsRequest {
                include_exhausted: false,
            })
            .await
            .expect("list grants")
            .grants
            .iter()
            .any(|g| g.grant_id == grant.grant_id)
    );
}

#[tokio::test]
async fn reload_controller_permissions_refreshes_controller_scope_without_a_session() {
    use crate::{
        FilePermissionMatcher, PathMatch, PermissionEffect, PermissionFileOperation,
        PermissionMatcher, PermissionRule,
    };
    use assistant_protocol::{ReloadControllerPermissionsRequest, RuntimeEvent};

    let source = Arc::new(MutablePermissionStore::writable());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            file_calls_step("ctrl-allow-after-reload", &["reloaded.txt"]),
            ModelScript::Events(message_events(&assistant_text("ctrl-done", "ok"))),
        ],
    ));
    let runtime = runtime_with_permission_components(source.clone(), model, file_tools()).await;
    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("controller")
        .session
        .session_id;

    let external_allow = PermissionDocument {
        schema_version: 2,
        rules: vec![PermissionRule {
            id: "external-ctrl-allow".to_owned(),
            effect: PermissionEffect::Allow,
            variants: vec![AgentVariant::Build],
            matcher: PermissionMatcher::File(FilePermissionMatcher {
                excluded_paths: Vec::new(),
                operation: PermissionFileOperation::Read,
                path: test_absolute_path("/workspace"),
                path_match: PathMatch::Recursive,
            }),
        }],
    }
    .render()
    .expect("render valid controller document");
    source.put(PermissionFileScope::Controller, external_allow);

    let mut events = runtime.subscribe_events();
    let result = runtime
        .reload_controller_permissions(ReloadControllerPermissionsRequest::default())
        .await
        .expect("reload controller permissions");
    assert!(result.applied);
    assert_eq!(result.file.scope, PermissionScope::Controller);
    assert_eq!(result.file.status, PermissionFileStatus::Ready);
    assert!(result.diagnostics.is_empty());
    assert!(matches!(
        events.recv().await.expect("permission changed event"),
        RuntimeEvent::PermissionChanged
    ));

    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "read reloaded controller permission".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit controller input")
        .run;
    let term = wait_for_terminal(&runtime, &controller, &run.run_id).await;
    assert_eq!(term.status, RunStatus::Completed);
    assert_eq!(
        term.tools[0].status,
        assistant_protocol::ToolActivityStatus::Completed
    );

    // Invalid external JSON keeps previously active controller snapshot intact.
    source.put(
        PermissionFileScope::Controller,
        br#"{"schema_version":2,"rules":["invalid"]}"#.to_vec(),
    );
    let invalid_reload = runtime
        .reload_controller_permissions(ReloadControllerPermissionsRequest::default())
        .await
        .expect("invalid reload returns diagnostics");
    assert!(!invalid_reload.applied);
    assert_eq!(invalid_reload.file.scope, PermissionScope::Controller);
    assert_eq!(invalid_reload.file.status, PermissionFileStatus::Invalid);
    assert!(!invalid_reload.diagnostics.is_empty());

    let snapshot = runtime
        .permission_coordinator
        .registry()
        .snapshot(&PermissionFileScope::Controller)
        .expect("registry")
        .expect("controller snapshot intact");
    assert_eq!(
        snapshot.document.as_ref().expect("valid prior doc").rules[0].id,
        "external-ctrl-allow"
    );
}
