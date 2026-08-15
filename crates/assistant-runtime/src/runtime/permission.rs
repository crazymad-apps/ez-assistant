//! 以 Session cohort 为边界的显式权限重载。

use assistant_protocol::{
    GetPermissionDocumentRequest, GetPermissionDocumentResult, GetWorkspaceRequest,
    PermissionDiagnostic, PermissionDocumentScope, ReloadPermissionsRequest,
    ReloadPermissionsResult, ReplacePermissionDocumentRequest, ReplacePermissionDocumentResult,
    RuntimeEvent,
};

use super::AssistantRuntime;
use crate::{
    RuntimeError, RuntimeResult,
    permission::{
        document_from_protocol, revision_from_protocol, scope_from_protocol, snapshot_from_load,
    },
};

impl AssistantRuntime {
    pub async fn get_permission_document(
        &self,
        request: GetPermissionDocumentRequest,
    ) -> RuntimeResult<GetPermissionDocumentResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.ensure_permission_scope_exists(&request.scope)?;
        let scope = scope_from_protocol(request.scope);
        let load = self.permission_coordinator.load_document(scope).await?;
        Ok(GetPermissionDocumentResult {
            document: snapshot_from_load(&load),
        })
    }

    pub async fn replace_permission_document(
        &self,
        request: ReplacePermissionDocumentRequest,
    ) -> RuntimeResult<ReplacePermissionDocumentResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if matches!(request.scope, PermissionDocumentScope::Global) {
            return Err(RuntimeError::InvalidRequest {
                reason: "global permission document is read-only",
            });
        }
        self.ensure_permission_scope_exists(&request.scope)?;
        let scope = scope_from_protocol(request.scope);
        let expected_revision = revision_from_protocol(request.expected_revision);
        let document = document_from_protocol(request.document);
        let candidate_rules = document.rules.clone();
        let load = self
            .permission_coordinator
            .replace_document(scope.clone(), expected_revision, document)
            .await?;
        self.drain_exact_rule_approvals(&scope, &candidate_rules);
        Ok(ReplacePermissionDocumentResult {
            document: snapshot_from_load(&load),
        })
    }

    pub async fn reload_permissions(
        &self,
        request: ReloadPermissionsRequest,
    ) -> RuntimeResult<ReloadPermissionsResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let outcome = self
            .permission_coordinator
            .reload(session.permission_scopes())
            .await?;
        let files = outcome
            .loads
            .iter()
            .map(|load| load.summary())
            .collect::<Vec<_>>();
        let diagnostics = outcome
            .loads
            .iter()
            .flat_map(|load| load.diagnostics.iter().cloned())
            .collect::<Vec<PermissionDiagnostic>>();
        if outcome.applied {
            self.publish(RuntimeEvent::PermissionReloaded {
                session_id: request.session_id.clone(),
                files: files.clone(),
            });
        }
        Ok(ReloadPermissionsResult {
            session_id: request.session_id,
            applied: outcome.applied,
            files,
            diagnostics,
        })
    }

    fn ensure_permission_scope_exists(&self, scope: &PermissionDocumentScope) -> RuntimeResult<()> {
        match scope {
            PermissionDocumentScope::Global => Ok(()),
            PermissionDocumentScope::Workspace { workspace_id } => self
                .get_workspace(GetWorkspaceRequest {
                    workspace_id: workspace_id.clone(),
                })
                .map(|_| ()),
            PermissionDocumentScope::Session { session_id } => self.session(session_id).map(|_| ()),
        }
    }
}
