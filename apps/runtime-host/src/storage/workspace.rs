//! Workspace canonical 身份、Agent 私有目录与假删原子操作。

use std::{fs, path::Path};

use assistant_protocol::WorkspaceId;
use assistant_runtime::{
    NewWorkspaceRegistration, StoreError, StoreErrorKind, StoredWorkspace,
    StoredWorkspaceLifecycle, WorkspaceRemoval, WorkspaceUpdate,
};
use rusqlite::{OptionalExtension, params};

use super::{
    StorageEngine, StorageResult, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, sync_directory,
};
use crate::config_source::prepare_private_directory;

impl StorageEngine {
    /// Runtime Home 整体移动后，重建只由 Workspace ID 决定的 Host 私有目录。
    /// 用户工作目录是外部资源，不参与重定位。
    pub(super) fn repair_workspace_resources(&mut self) -> StorageResult<()> {
        let rows = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT workspace_id, agent_directory FROM workspaces ORDER BY workspace_id",
                )
                .map_err(|source| {
                    internal_error("workspace resources could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| {
                    internal_error("workspace resources could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("workspace resource row could not be read", source)
            })?
        };
        for (workspace_id, stored_agent_directory) in rows {
            let workspace_id = WorkspaceId::new(workspace_id)
                .map_err(|_| invalid_data("stored workspace id is invalid"))?;
            super::filesystem::validate_workspace_component(&workspace_id)?;
            let expected = self
                .workspaces_directory
                .join(workspace_id.as_str())
                .join("agent");
            let expected_text = expected
                .to_str()
                .ok_or_else(|| invalid_data("workspace agent directory is not valid UTF-8"))?;
            if stored_agent_directory == expected_text {
                continue;
            }
            if !is_moved_workspace_agent_directory(
                Path::new(&stored_agent_directory),
                workspace_id.as_str(),
            ) {
                return Err(invalid_data("stored workspace agent directory is invalid"));
            }
            self.connection
                .execute(
                    "UPDATE workspaces SET agent_directory = ?1 WHERE workspace_id = ?2",
                    params![expected_text, workspace_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("workspace resource path could not be rebased", source)
                })?;
        }
        Ok(())
    }

    pub(super) fn register_workspace(
        &mut self,
        registration: NewWorkspaceRegistration,
    ) -> StorageResult<StoredWorkspace> {
        super::filesystem::validate_workspace_component(&registration.workspace_id)?;
        let (canonical, additional_directories) = canonicalize_workspace_directories(
            &registration.requested_primary_directory,
            &registration.requested_additional_directories,
        )?;

        if let Some(mut existing) = self.workspace_by_user_directory(&canonical)? {
            self.prepare_workspace_agent_directory(&existing)?;
            let _ = self.reconcile_legacy_workspace_permission_file(&existing)?;
            let additional_json =
                serde_json::to_string(&additional_directories).map_err(|source| {
                    internal_error("workspace directories could not be encoded", source)
                })?;
            self.connection
                .execute(
                    "UPDATE workspaces
                     SET label = ?1, additional_directories_json = ?2, lifecycle = 'active',
                         updated_at_ms = ?3, removed_at_ms = NULL
                     WHERE workspace_id = ?4",
                    params![
                        registration.label,
                        additional_json,
                        registration.changed_at_ms,
                        existing.workspace_id.as_str()
                    ],
                )
                .map_err(|source| {
                    database_write_error("workspace could not be restored", source)
                })?;
            existing.label = registration.label;
            existing.additional_directories = additional_directories;
            existing.lifecycle = StoredWorkspaceLifecycle::Active;
            existing.updated_at_ms = registration.changed_at_ms;
            existing.removed_at_ms = None;
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
        let workspace = StoredWorkspace {
            workspace_id: registration.workspace_id,
            label: registration.label,
            user_directory: canonical.clone(),
            additional_directories: additional_directories.clone(),
            agent_directory: agent_directory_text.to_owned(),
            lifecycle: StoredWorkspaceLifecycle::Active,
            created_at_ms: registration.changed_at_ms,
            updated_at_ms: registration.changed_at_ms,
            removed_at_ms: None,
        };
        let additional_json = serde_json::to_string(&additional_directories).map_err(|source| {
            internal_error("workspace directories could not be encoded", source)
        })?;
        let inserted = self.connection.execute(
            "INSERT INTO workspaces (
                workspace_id, label, user_directory, additional_directories_json,
                agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, NULL)",
            params![
                workspace.workspace_id.as_str(),
                workspace.label,
                canonical,
                additional_json,
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
        Ok(workspace)
    }

    pub(super) fn update_workspace(
        &mut self,
        update: WorkspaceUpdate,
    ) -> StorageResult<StoredWorkspace> {
        super::filesystem::validate_workspace_component(&update.workspace_id)?;
        let mut workspace = self.get_workspace(&update.workspace_id)?;
        if workspace.lifecycle != StoredWorkspaceLifecycle::Active {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "removed workspace cannot be updated",
            ));
        }
        let (primary_directory, additional_directories) = canonicalize_workspace_directories(
            &update.requested_primary_directory,
            &update.requested_additional_directories,
        )?;
        if let Some(existing) = self.workspace_by_user_directory(&primary_directory)?
            && existing.workspace_id != update.workspace_id
        {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "workspace primary directory belongs to another workspace",
            ));
        }
        let additional_json = serde_json::to_string(&additional_directories).map_err(|source| {
            internal_error("workspace directories could not be encoded", source)
        })?;
        self.connection
            .execute(
                "UPDATE workspaces
                 SET label = ?1, user_directory = ?2, additional_directories_json = ?3,
                     updated_at_ms = ?4
                 WHERE workspace_id = ?5 AND lifecycle = 'active'",
                params![
                    update.label,
                    primary_directory,
                    additional_json,
                    update.changed_at_ms,
                    update.workspace_id.as_str(),
                ],
            )
            .map_err(|source| database_write_error("workspace could not be updated", source))?;
        workspace.label = update.label;
        workspace.user_directory = primary_directory;
        workspace.additional_directories = additional_directories;
        workspace.updated_at_ms = update.changed_at_ms;
        Ok(workspace)
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
            let _ = self.reconcile_legacy_workspace_permission_file(workspace)?;
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
            "SELECT workspace_id, label, user_directory, additional_directories_json,
                    agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
             FROM workspaces ORDER BY workspace_id"
        } else {
            "SELECT workspace_id, label, user_directory, additional_directories_json,
                    agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
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
                "SELECT workspace_id, label, user_directory, additional_directories_json,
                        agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
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
                "SELECT workspace_id, label, user_directory, additional_directories_json,
                        agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
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

fn canonicalize_workspace_directories(
    primary_directory: &str,
    additional_directories: &[String],
) -> StorageResult<(String, Vec<String>)> {
    let mut canonical = Vec::with_capacity(additional_directories.len() + 1);
    for requested in
        std::iter::once(primary_directory).chain(additional_directories.iter().map(String::as_str))
    {
        let requested = Path::new(requested);
        if !requested.is_absolute() {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "workspace path must be absolute",
            ));
        }
        let directory = fs::canonicalize(requested).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
                source,
            )
        })?;
        let metadata = fs::metadata(&directory).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
                source,
            )
        })?;
        if !metadata.is_dir() || fs::read_dir(&directory).is_err() {
            return Err(StoreError::new(
                StoreErrorKind::ResourceUnavailable,
                "workspace directory is unavailable",
            ));
        }
        let directory = directory.to_str().ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::InvalidInput,
                "workspace path must be valid UTF-8",
            )
        })?;
        if canonical.iter().any(|existing| existing == directory) {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "workspace directories must be unique",
            ));
        }
        canonical.push(directory.to_owned());
    }
    let primary = canonical.remove(0);
    Ok((primary, canonical))
}

fn is_moved_workspace_agent_directory(path: &Path, workspace_id: &str) -> bool {
    path.is_absolute()
        && path.file_name().and_then(|value| value.to_str()) == Some("agent")
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            == Some(workspace_id)
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            == Some("workspaces")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            == Some("data")
}

type WorkspaceRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
);

fn read_workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn parse_workspace_row(row: WorkspaceRow) -> StorageResult<StoredWorkspace> {
    let (
        workspace_id,
        label,
        user_directory,
        additional_directories_json,
        agent_directory,
        lifecycle,
        created,
        updated,
        removed,
    ) = row;
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
    if label.is_empty() {
        return Err(invalid_data("stored workspace label is invalid"));
    }
    let additional_directories = super::decode_additional_directories(
        &additional_directories_json,
        "stored workspace additional directories are invalid",
    )?;
    Ok(StoredWorkspace {
        workspace_id,
        label,
        user_directory,
        additional_directories,
        agent_directory,
        lifecycle,
        created_at_ms: created,
        updated_at_ms: updated,
        removed_at_ms: removed,
    })
}
