use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use assistant_protocol::{
    CreateSessionRequest, PermissionDiagnosticCode, PermissionFileStatus, PermissionScope,
    RegisterWorkspaceRequest, ReloadPermissionsRequest, RuntimeEvent,
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
                    revision: PermissionFileRevision::Content(format!("test-{}", content.len())),
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
                    let revision =
                        PermissionFileRevision::Content(format!("test-{}", content.len()));
                    self.files
                        .lock()
                        .map_err(|_| {
                            StoreError::new(StoreErrorKind::Unavailable, "file lock failed")
                        })?
                        .insert(scope.clone(), content);
                    Ok(revision)
                }
            });
        Box::pin(async move { result })
    }
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
