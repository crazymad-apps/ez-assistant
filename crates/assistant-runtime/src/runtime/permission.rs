//! 以 Session cohort 为边界的显式权限重载。

use assistant_protocol::{
    PermissionDiagnostic, ReloadPermissionsRequest, ReloadPermissionsResult, RuntimeEvent,
};

use super::AssistantRuntime;
use crate::RuntimeResult;

impl AssistantRuntime {
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
}
