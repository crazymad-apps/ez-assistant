use super::*;

impl VolatileRuntimeStore {
    pub(super) fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state.workspaces.values_mut().find(|workspace| {
                workspace.user_directory == registration.requested_primary_directory
            }) {
                existing.lifecycle = StoredWorkspaceLifecycle::Active;
                existing.label = registration.label;
                existing.additional_directories = registration.requested_additional_directories;
                existing.updated_at_ms = registration.changed_at_ms;
                existing.removed_at_ms = None;
                return Ok(existing.clone());
            }
            let stored = StoredWorkspace {
                workspace_id: registration.workspace_id.clone(),
                label: registration.label,
                user_directory: registration.requested_primary_directory,
                additional_directories: registration.requested_additional_directories,
                agent_directory: format!(
                    "{VOLATILE_ROOT}/workspaces/{}/agent",
                    registration.workspace_id
                ),
                lifecycle: StoredWorkspaceLifecycle::Active,
                created_at_ms: registration.changed_at_ms,
                updated_at_ms: registration.changed_at_ms,
                removed_at_ms: None,
            };
            if state
                .workspaces
                .insert(registration.workspace_id, stored.clone())
                .is_some()
            {
                return Err(conflict("workspace already exists in runtime storage"));
            }
            Ok(stored)
        })
    }

    pub(super) fn update_workspace(
        &self,
        update: WorkspaceUpdate,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if state.workspaces.values().any(|workspace| {
                workspace.workspace_id != update.workspace_id
                    && workspace.lifecycle == StoredWorkspaceLifecycle::Active
                    && workspace.user_directory == update.requested_primary_directory
            }) {
                return Err(conflict(
                    "workspace primary directory is already registered",
                ));
            }
            let workspace = state
                .workspaces
                .get_mut(&update.workspace_id)
                .ok_or_else(|| conflict("workspace does not exist in runtime storage"))?;
            if workspace.lifecycle != StoredWorkspaceLifecycle::Active {
                return Err(conflict("removed workspace cannot be updated"));
            }
            workspace.label = update.label;
            workspace.user_directory = update.requested_primary_directory;
            workspace.additional_directories = update.requested_additional_directories;
            workspace.updated_at_ms = update.changed_at_ms;
            Ok(workspace.clone())
        })
    }

    pub(super) fn remove_workspace(
        &self,
        removal: WorkspaceRemoval,
    ) -> StoreFuture<'_, StoredWorkspace> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let workspace = state
                .workspaces
                .get_mut(&removal.workspace_id)
                .ok_or_else(|| conflict("workspace does not exist in runtime storage"))?;
            if workspace.lifecycle == StoredWorkspaceLifecycle::Active {
                workspace.lifecycle = StoredWorkspaceLifecycle::Removed;
                workspace.updated_at_ms = removal.changed_at_ms;
                workspace.removed_at_ms = Some(removal.changed_at_ms);
            }
            Ok(workspace.clone())
        })
    }

    pub(super) fn upload_attachment(
        &self,
        upload: NewAttachmentUpload,
    ) -> StoreFuture<'_, StoredAttachment> {
        Box::pin(async move {
            let mut state = self.lock()?;
            if let Some(existing) = state.attachments.values().find(|attachment| {
                attachment.session_id == upload.session_id
                    && attachment.blob_hash == upload.blob_hash
            }) {
                return Ok(existing.clone());
            }
            if !state.sessions.contains_key(&upload.session_id) {
                return Err(conflict("attachment session does not exist"));
            }
            let agent_readable_path = format!(
                "{VOLATILE_ROOT}/sessions/{}/attachments/{}/file",
                upload.session_id, upload.attachment_id
            );
            let stored = StoredAttachment {
                attachment_id: upload.attachment_id.clone(),
                session_id: upload.session_id,
                original_name: upload.original_name,
                blob_hash: upload.blob_hash,
                size_bytes: upload.size_bytes,
                media_type: upload.media_type,
                agent_readable_path,
                state: StoredAttachmentState::Ready,
                created_at_ms: upload.created_at_ms,
            };
            if state
                .attachments
                .insert(upload.attachment_id, stored.clone())
                .is_some()
            {
                return Err(conflict("attachment already exists"));
            }
            Ok(stored)
        })
    }
}
