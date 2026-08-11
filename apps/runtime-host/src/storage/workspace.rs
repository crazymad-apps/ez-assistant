//! Workspace canonical 身份、Agent 私有目录与假删原子操作。

use std::{fs, path::Path};

use assistant_protocol::WorkspaceId;
use assistant_runtime::{
    NewWorkspaceRegistration, StoreError, StoreErrorKind, StoredWorkspace,
    StoredWorkspaceLifecycle, WorkspaceRemoval,
};
use rusqlite::{OptionalExtension, params};

use super::{
    StorageEngine, StorageResult, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, sync_directory,
};
use crate::config_source::prepare_private_directory;

impl StorageEngine {
    pub(super) fn register_workspace(
        &mut self,
        registration: NewWorkspaceRegistration,
    ) -> StorageResult<StoredWorkspace> {
        super::filesystem::validate_workspace_component(&registration.workspace_id)?;
        let requested = Path::new(&registration.requested_directory);
        if !requested.is_absolute() {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "workspace path must be absolute",
            ));
        }
        let canonical = fs::canonicalize(requested).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
                source,
            )
        })?;
        let metadata = fs::metadata(&canonical).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
                source,
            )
        })?;
        if !metadata.is_dir() {
            return Err(StoreError::new(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
            ));
        }
        let canonical = canonical.to_str().ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::InvalidInput,
                "workspace path must be valid UTF-8",
            )
        })?;

        if let Some(mut existing) = self.workspace_by_user_directory(canonical)? {
            self.prepare_workspace_agent_directory(&existing)?;
            if existing.lifecycle == StoredWorkspaceLifecycle::Removed {
                self.connection
                    .execute(
                        "UPDATE workspaces
                         SET lifecycle = 'active', updated_at_ms = ?1, removed_at_ms = NULL
                         WHERE workspace_id = ?2 AND lifecycle = 'removed'",
                        params![registration.changed_at_ms, existing.workspace_id.as_str()],
                    )
                    .map_err(|source| {
                        database_write_error("workspace could not be restored", source)
                    })?;
                existing.lifecycle = StoredWorkspaceLifecycle::Active;
                existing.updated_at_ms = registration.changed_at_ms;
                existing.removed_at_ms = None;
            }
            return Ok(existing);
        }

        let workspace_directory = self
            .workspaces_directory
            .join(registration.workspace_id.as_str());
        let agent_directory = workspace_directory.join("agent");
        prepare_private_directory(&workspace_directory).map_err(|source| {
            internal_error("workspace data directory could not be prepared", source)
        })?;
        prepare_private_directory(&agent_directory).map_err(|source| {
            internal_error("workspace agent directory could not be prepared", source)
        })?;
        let agent_directory_text = agent_directory
            .to_str()
            .ok_or_else(|| invalid_data("workspace agent directory is not valid UTF-8"))?;
        let inserted = self.connection.execute(
            "INSERT INTO workspaces (
                workspace_id, user_directory, agent_directory, lifecycle,
                created_at_ms, updated_at_ms, removed_at_ms
             ) VALUES (?1, ?2, ?3, 'active', ?4, ?4, NULL)",
            params![
                registration.workspace_id.as_str(),
                canonical,
                agent_directory_text,
                registration.changed_at_ms,
            ],
        );
        if let Err(source) = inserted {
            let _ = fs::remove_dir(&agent_directory);
            let _ = fs::remove_dir(&workspace_directory);
            return Err(database_write_error(
                "workspace could not be created in runtime storage",
                source,
            ));
        }
        sync_directory(&self.workspaces_directory)?;
        Ok(StoredWorkspace {
            workspace_id: registration.workspace_id,
            user_directory: canonical.to_owned(),
            agent_directory: agent_directory_text.to_owned(),
            lifecycle: StoredWorkspaceLifecycle::Active,
            created_at_ms: registration.changed_at_ms,
            updated_at_ms: registration.changed_at_ms,
            removed_at_ms: None,
        })
    }

    pub(super) fn get_workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> StorageResult<StoredWorkspace> {
        super::filesystem::validate_workspace_component(workspace_id)?;
        self.workspace_by_id(workspace_id)?.ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::Conflict,
                "workspace does not exist in runtime storage",
            )
        })
    }

    pub(super) fn load_all_workspaces(&self) -> StorageResult<Vec<StoredWorkspace>> {
        let workspaces = self.load_workspaces(true)?;
        for workspace in &workspaces {
            self.prepare_workspace_agent_directory(workspace)?;
        }
        Ok(workspaces)
    }

    pub(super) fn remove_workspace(
        &mut self,
        removal: WorkspaceRemoval,
    ) -> StorageResult<StoredWorkspace> {
        let mut workspace = self.get_workspace(&removal.workspace_id)?;
        if workspace.lifecycle == StoredWorkspaceLifecycle::Active {
            self.connection
                .execute(
                    "UPDATE workspaces
                     SET lifecycle = 'removed', updated_at_ms = ?1, removed_at_ms = ?1
                     WHERE workspace_id = ?2 AND lifecycle = 'active'",
                    params![removal.changed_at_ms, removal.workspace_id.as_str()],
                )
                .map_err(|source| database_write_error("workspace could not be removed", source))?;
            workspace.lifecycle = StoredWorkspaceLifecycle::Removed;
            workspace.updated_at_ms = removal.changed_at_ms;
            workspace.removed_at_ms = Some(removal.changed_at_ms);
        }
        Ok(workspace)
    }

    fn load_workspaces(&self, include_removed: bool) -> StorageResult<Vec<StoredWorkspace>> {
        let sql = if include_removed {
            "SELECT workspace_id, user_directory, agent_directory, lifecycle,
                    created_at_ms, updated_at_ms, removed_at_ms
             FROM workspaces ORDER BY workspace_id"
        } else {
            "SELECT workspace_id, user_directory, agent_directory, lifecycle,
                    created_at_ms, updated_at_ms, removed_at_ms
             FROM workspaces WHERE lifecycle = 'active' ORDER BY workspace_id"
        };
        let mut statement = self
            .connection
            .prepare(sql)
            .map_err(|source| internal_error("runtime workspaces could not be queried", source))?;
        let rows = statement
            .query_map([], read_workspace_row)
            .map_err(|source| internal_error("runtime workspaces could not be read", source))?;
        rows.map(|row| {
            row.map_err(|source| internal_error("runtime workspace row could not be read", source))
                .and_then(parse_workspace_row)
        })
        .collect()
    }

    fn workspace_by_id(
        &self,
        workspace_id: &WorkspaceId,
    ) -> StorageResult<Option<StoredWorkspace>> {
        self.connection
            .query_row(
                "SELECT workspace_id, user_directory, agent_directory, lifecycle,
                        created_at_ms, updated_at_ms, removed_at_ms
                 FROM workspaces WHERE workspace_id = ?1",
                [workspace_id.as_str()],
                read_workspace_row,
            )
            .optional()
            .map_err(|source| internal_error("runtime workspace could not be queried", source))?
            .map(parse_workspace_row)
            .transpose()
    }

    fn workspace_by_user_directory(
        &self,
        user_directory: &str,
    ) -> StorageResult<Option<StoredWorkspace>> {
        self.connection
            .query_row(
                "SELECT workspace_id, user_directory, agent_directory, lifecycle,
                        created_at_ms, updated_at_ms, removed_at_ms
                 FROM workspaces WHERE user_directory = ?1",
                [user_directory],
                read_workspace_row,
            )
            .optional()
            .map_err(|source| internal_error("runtime workspace could not be queried", source))?
            .map(parse_workspace_row)
            .transpose()
    }

    fn prepare_workspace_agent_directory(&self, workspace: &StoredWorkspace) -> StorageResult<()> {
        super::filesystem::validate_workspace_component(&workspace.workspace_id)?;
        let expected = self
            .workspaces_directory
            .join(workspace.workspace_id.as_str())
            .join("agent");
        if expected.to_str() != Some(workspace.agent_directory.as_str()) {
            return Err(invalid_data("stored workspace agent directory is invalid"));
        }
        let parent = expected
            .parent()
            .ok_or_else(|| invalid_data("stored workspace agent directory is invalid"))?;
        prepare_private_directory(parent).map_err(|source| {
            internal_error("workspace data directory could not be prepared", source)
        })?;
        prepare_private_directory(&expected).map_err(|source| {
            internal_error("workspace agent directory could not be prepared", source)
        })
    }
}

type WorkspaceRow = (String, String, String, String, i64, i64, Option<i64>);

fn read_workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn parse_workspace_row(row: WorkspaceRow) -> StorageResult<StoredWorkspace> {
    let (workspace_id, user_directory, agent_directory, lifecycle, created, updated, removed) = row;
    let workspace_id = WorkspaceId::new(workspace_id)
        .map_err(|source| invalid_data_with_source("stored workspace id is invalid", source))?;
    let lifecycle = match lifecycle.as_str() {
        "active" => StoredWorkspaceLifecycle::Active,
        "removed" => StoredWorkspaceLifecycle::Removed,
        _ => return Err(invalid_data("stored workspace lifecycle is invalid")),
    };
    if !Path::new(&user_directory).is_absolute() || !Path::new(&agent_directory).is_absolute() {
        return Err(invalid_data("stored workspace path is invalid"));
    }
    Ok(StoredWorkspace {
        workspace_id,
        user_directory,
        agent_directory,
        lifecycle,
        created_at_ms: created,
        updated_at_ms: updated,
        removed_at_ms: removed,
    })
}
