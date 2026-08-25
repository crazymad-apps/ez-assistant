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
        let path = AbsolutePath::new(std::path::Path::new("/workspace").join(input.path))
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
            path: "/workspace/permission-reload".to_owned(),
        })
        .await
        .expect("register workspace")
        .workspace;
    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
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
                path: "/private/session".to_owned(),
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
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "read twice".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
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
                        path: "/workspace/repeated.txt".to_owned(),
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
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "read once".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
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
                        path: "/workspace".to_owned(),
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
