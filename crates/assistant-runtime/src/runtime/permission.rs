//! 以 Session cohort 为边界的显式权限重载。

use assistant_protocol::{
    CreatePermissionGrantRequest, CreatePermissionGrantResult, GetPermissionDocumentRequest,
    GetPermissionDocumentResult, GetWorkspaceRequest, ListPermissionGrantsRequest,
    ListPermissionGrantsResult, PermissionDiagnostic, PermissionDocumentScope,
    PermissionFileStatus, PermissionFileSummary, PermissionGrantSnapshot, PermissionScope,
    ReloadControllerPermissionsRequest, ReloadControllerPermissionsResult,
    ReloadPermissionsRequest, ReloadPermissionsResult, ReplacePermissionDocumentRequest,
    ReplacePermissionDocumentResult, RevokePermissionGrantRequest, RevokePermissionGrantResult,
    RuntimeEvent,
};

use super::{AssistantRuntime, now_ms};
use crate::{
    PermissionDocument, PermissionEffect, PermissionFileScope, PermissionRule, RuntimeError,
    RuntimeResult, StoredPermissionGrant,
    permission::{
        document_from_protocol, matcher_from_protocol, matcher_to_protocol, revision_from_protocol,
        scope_from_protocol, snapshot_from_load,
    },
};

impl AssistantRuntime {
    pub async fn get_permission_document(
        &self,
        request: GetPermissionDocumentRequest,
    ) -> RuntimeResult<GetPermissionDocumentResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.ensure_permission_scope_exists(&request.scope).await?;
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
        self.ensure_permission_scope_exists(&request.scope).await?;
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
        let session = self.session(&request.session_id).await?;
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

    pub async fn reload_controller_permissions(
        &self,
        _request: ReloadControllerPermissionsRequest,
    ) -> RuntimeResult<ReloadControllerPermissionsResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let outcome = self
            .permission_coordinator
            .reload(vec![PermissionFileScope::Controller])
            .await?;
        let file =
            outcome
                .loads
                .first()
                .map(|load| load.summary())
                .unwrap_or(PermissionFileSummary {
                    scope: PermissionScope::Controller,
                    status: PermissionFileStatus::Unavailable,
                });
        let diagnostics = outcome
            .loads
            .iter()
            .flat_map(|load| load.diagnostics.iter().cloned())
            .collect::<Vec<PermissionDiagnostic>>();
        if outcome.applied {
            self.publish(RuntimeEvent::PermissionChanged);
        }
        Ok(ReloadControllerPermissionsResult {
            applied: outcome.applied,
            file,
            diagnostics,
        })
    }

    pub async fn list_permission_grants(
        &self,
        request: ListPermissionGrantsRequest,
    ) -> RuntimeResult<ListPermissionGrantsResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let grants = self
            .store
            .list_permission_grants(request.include_exhausted)
            .await
            .map_err(|source| RuntimeError::from_store("list permission grants", source))?;
        let current_ms = if request.include_exhausted {
            None
        } else {
            Some(now_ms()?)
        };
        Ok(ListPermissionGrantsResult {
            grants: grants
                .iter()
                .filter(|grant| {
                    current_ms.is_none_or(|now| !grant.expires_at_ms.is_some_and(|exp| exp <= now))
                })
                .map(project_permission_grant)
                .collect(),
        })
    }

    pub async fn create_permission_grant(
        &self,
        request: CreatePermissionGrantRequest,
    ) -> RuntimeResult<CreatePermissionGrantResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if request.scope != PermissionScope::Controller {
            return Err(RuntimeError::InvalidRequest {
                reason: "pre-authorized permission grants only support controller scope",
            });
        }
        if !(1..=100).contains(&request.max_uses) {
            return Err(RuntimeError::InvalidRequest {
                reason: "permission grant max_uses must be between 1 and 100",
            });
        }
        if request.variants.is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "permission grant variants must not be empty",
            });
        }
        if request.note.len() > 256
            || request
                .note
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "permission grant note is invalid or too long",
            });
        }
        let ttl_ms = request
            .ttl_seconds
            .map(|seconds| {
                if !(1..=604_800).contains(&seconds) {
                    return Err(RuntimeError::InvalidRequest {
                        reason: "permission grant ttl_seconds must be between 1 and 604800",
                    });
                }
                i64::try_from(seconds.saturating_mul(1_000)).map_err(|_| {
                    RuntimeError::InvalidRequest {
                        reason: "permission grant ttl_seconds overflows timestamp range",
                    }
                })
            })
            .transpose()?;
        let matcher = matcher_from_protocol(request.matcher);
        PermissionDocument {
            schema_version: 2,
            rules: vec![PermissionRule {
                id: "grant".to_owned(),
                effect: PermissionEffect::Allow,
                variants: request.variants.clone(),
                matcher: matcher.clone(),
            }],
        }
        .validate()
        .map_err(|_| RuntimeError::InvalidRequest {
            reason: "permission grant matcher is invalid",
        })?;
        let created_at_ms = now_ms()?;
        let expires_at_ms = ttl_ms
            .map(|delta| {
                created_at_ms
                    .checked_add(delta)
                    .ok_or(RuntimeError::InvalidRequest {
                        reason: "permission grant expiry overflows timestamp range",
                    })
            })
            .transpose()?;
        let grant_id =
            crate::id::generate("pg").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "permission grant id generator",
            })?;
        let stored = StoredPermissionGrant {
            grant_id,
            scope: request.scope,
            variants: request.variants,
            matcher,
            max_uses: request.max_uses,
            remaining_uses: request.max_uses,
            expires_at_ms,
            note: request.note,
            created_at_ms,
            consumed_at_ms: None,
        };
        let persisted = self
            .store
            .create_permission_grant(stored)
            .await
            .map_err(|source| RuntimeError::from_store("create permission grant", source))?;
        self.publish(RuntimeEvent::PermissionGrantsChanged);
        Ok(CreatePermissionGrantResult {
            grant: project_permission_grant(&persisted),
        })
    }

    pub async fn revoke_permission_grant(
        &self,
        request: RevokePermissionGrantRequest,
    ) -> RuntimeResult<RevokePermissionGrantResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let grant_id = request.grant_id.trim().to_owned();
        if grant_id.is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "permission grant id must not be empty",
            });
        }
        let revoked = self
            .store
            .revoke_permission_grant(&grant_id)
            .await
            .map_err(|source| RuntimeError::from_store("revoke permission grant", source))?;
        if revoked {
            self.publish(RuntimeEvent::PermissionGrantsChanged);
        }
        Ok(RevokePermissionGrantResult { revoked, grant_id })
    }

    async fn ensure_permission_scope_exists(
        &self,
        scope: &PermissionDocumentScope,
    ) -> RuntimeResult<()> {
        match scope {
            PermissionDocumentScope::Global | PermissionDocumentScope::Controller => Ok(()),
            PermissionDocumentScope::Workspace { workspace_id } => self
                .get_workspace(GetWorkspaceRequest {
                    workspace_id: workspace_id.clone(),
                })
                .map(|_| ()),
            PermissionDocumentScope::Session { session_id } => {
                self.session(session_id).await.map(|_| ())
            }
        }
    }
}

fn project_permission_grant(grant: &StoredPermissionGrant) -> PermissionGrantSnapshot {
    PermissionGrantSnapshot {
        grant_id: grant.grant_id.clone(),
        scope: grant.scope,
        variants: grant.variants.clone(),
        matcher: matcher_to_protocol(&grant.matcher),
        max_uses: grant.max_uses,
        remaining_uses: grant.remaining_uses,
        expires_at_ms: grant.expires_at_ms,
        note: grant.note.clone(),
        created_at_ms: grant.created_at_ms,
        consumed_at_ms: grant.consumed_at_ms,
    }
}
