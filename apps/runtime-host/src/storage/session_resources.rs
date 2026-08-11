//! Session 冻结目录关系、旧数据补建与新 Session 环境校验。

use std::{fs, path::Path};

use assistant_protocol::{SessionId, WorkspaceId};
use assistant_runtime::{
    NewStoredSession, SessionExecutionEnvironment, StoreError, StoreErrorKind,
    StoredWorkspaceLifecycle,
};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{StorageEngine, StorageResult, internal_error, invalid_data};
use crate::config_source::prepare_private_directory;

pub(super) struct PreparedSessionDirectories {
    pub session_directory: std::path::PathBuf,
    pub attachment_directory: std::path::PathBuf,
    pub private_directory: std::path::PathBuf,
}

impl StorageEngine {
    /// 为 v0.10.0 Session 幂等补建 unbound 资源行，不改写 Prompt 或 Conversation。
    pub(super) fn repair_session_resources(&mut self) -> StorageResult<()> {
        let missing = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT s.session_id, s.created_at_ms
                     FROM sessions s
                     LEFT JOIN session_resources r ON r.session_id = s.session_id
                     WHERE r.session_id IS NULL
                     ORDER BY s.session_id",
                )
                .map_err(|source| {
                    internal_error("session resources could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|source| internal_error("session resources could not be read", source))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("session resource row could not be read", source)
            })?
        };

        for (session_id, created_at_ms) in missing {
            let session_id = SessionId::new(session_id)
                .map_err(|_| invalid_data("stored session id is invalid"))?;
            let paths = self.prepare_session_directories(&session_id)?;
            let private = path_text(&paths.private_directory)?;
            let attachment = path_text(&paths.attachment_directory)?;
            self.connection
                .execute(
                    "INSERT OR IGNORE INTO session_resources (
                        session_id, workspace_id, working_directory,
                        attachment_directory, private_directory, created_at_ms
                     ) VALUES (?1, NULL, ?2, ?3, ?2, ?4)",
                    params![session_id.as_str(), private, attachment, created_at_ms],
                )
                .map_err(|source| {
                    super::database_write_error("session resources could not be repaired", source)
                })?;
        }
        self.recover_session_resource_directories()
    }

    pub(super) fn prepare_new_session_directories(
        &self,
        session: &NewStoredSession,
    ) -> StorageResult<PreparedSessionDirectories> {
        self.validate_new_session_environment(session)?;
        self.prepare_session_directories(&session.session_id)
    }

    pub(super) fn insert_session_resources(
        transaction: &Transaction<'_>,
        session: &NewStoredSession,
    ) -> StorageResult<()> {
        transaction
            .execute(
                "INSERT INTO session_resources (
                    session_id, workspace_id, working_directory,
                    attachment_directory, private_directory, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    session.session_id.as_str(),
                    session
                        .environment
                        .workspace_id
                        .as_ref()
                        .map(WorkspaceId::as_str),
                    session.environment.working_directory,
                    session.environment.session_attachment_directory,
                    session.environment.session_private_directory,
                    session.created_at_ms,
                ],
            )
            .map_err(|source| {
                super::database_write_error("session resources could not be created", source)
            })?;
        Ok(())
    }

    pub(super) fn load_session_environment(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<SessionExecutionEnvironment> {
        let row = self
            .connection
            .query_row(
                "SELECT r.workspace_id, r.working_directory, w.user_directory,
                        w.agent_directory, r.attachment_directory, r.private_directory
                 FROM session_resources r
                 LEFT JOIN workspaces w ON w.workspace_id = r.workspace_id
                 WHERE r.session_id = ?1",
                [session_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("session environment could not be queried", source))?
            .ok_or_else(|| invalid_data("stored session environment is missing"))?;
        let workspace_id = row
            .0
            .map(|value| {
                WorkspaceId::new(value).map_err(|_| invalid_data("stored workspace id is invalid"))
            })
            .transpose()?;
        let workspace_private_directory = match workspace_id.as_ref() {
            Some(_) => {
                let user_directory = row
                    .2
                    .as_deref()
                    .ok_or_else(|| invalid_data("stored session workspace is invalid"))?;
                if row.1 != user_directory || row.3.is_none() {
                    return Err(invalid_data("stored session workspace is invalid"));
                }
                row.3
            }
            None => {
                if row.2.is_some() || row.3.is_some() || row.1 != row.5 {
                    return Err(invalid_data(
                        "stored unbound session environment is invalid",
                    ));
                }
                None
            }
        };
        let environment = SessionExecutionEnvironment {
            workspace_id,
            working_directory: row.1,
            workspace_private_directory,
            session_attachment_directory: row.4,
            session_private_directory: row.5,
        };
        self.validate_stored_session_environment(session_id, &environment)?;
        Ok(environment)
    }

    fn validate_new_session_environment(&self, session: &NewStoredSession) -> StorageResult<()> {
        self.validate_stored_session_environment(&session.session_id, &session.environment)?;
        match session.environment.workspace_id.as_ref() {
            Some(workspace_id) => {
                let workspace = self.get_workspace(workspace_id)?;
                if workspace.lifecycle != StoredWorkspaceLifecycle::Active {
                    return Err(StoreError::new(
                        StoreErrorKind::Conflict,
                        "workspace cannot accept new sessions",
                    ));
                }
                if session.environment.working_directory != workspace.user_directory
                    || session.environment.workspace_private_directory.as_deref()
                        != Some(workspace.agent_directory.as_str())
                {
                    return Err(StoreError::new(
                        StoreErrorKind::InvalidInput,
                        "session workspace environment does not match storage",
                    ));
                }
                if !fs::metadata(&workspace.user_directory)
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false)
                {
                    return Err(StoreError::new(
                        StoreErrorKind::ResourceUnavailable,
                        "workspace directory is unavailable",
                    ));
                }
            }
            None => {
                if session.environment.workspace_private_directory.is_some()
                    || session.environment.working_directory
                        != session.environment.session_private_directory
                {
                    return Err(StoreError::new(
                        StoreErrorKind::InvalidInput,
                        "unbound session environment is invalid",
                    ));
                }
            }
        }
        Ok(())
    }

    fn recover_session_resource_directories(&self) -> StorageResult<()> {
        let session_ids = {
            let mut statement = self
                .connection
                .prepare("SELECT session_id FROM sessions ORDER BY session_id")
                .map_err(|source| {
                    internal_error("session resources could not be queried", source)
                })?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| internal_error("session resources could not be read", source))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("session resource row could not be read", source)
            })?
        };
        for session_id in session_ids {
            let session_id = SessionId::new(session_id)
                .map_err(|_| invalid_data("stored session id is invalid"))?;
            let environment = self.load_session_environment(&session_id)?;
            let paths = self.prepare_session_directories(&session_id)?;
            if path_text(&paths.attachment_directory)? != environment.session_attachment_directory
                || path_text(&paths.private_directory)? != environment.session_private_directory
            {
                return Err(invalid_data("stored session directory is invalid"));
            }
        }
        Ok(())
    }

    fn validate_stored_session_environment(
        &self,
        session_id: &SessionId,
        environment: &SessionExecutionEnvironment,
    ) -> StorageResult<()> {
        let session_directory = self.session_directory(session_id)?;
        let expected_attachment = session_directory.join("attachments");
        let expected_private = session_directory.join("private");
        if path_text(&expected_attachment)? != environment.session_attachment_directory
            || path_text(&expected_private)? != environment.session_private_directory
            || !Path::new(&environment.working_directory).is_absolute()
            || environment
                .workspace_private_directory
                .as_deref()
                .is_some_and(|path| !Path::new(path).is_absolute())
        {
            return Err(invalid_data("stored session environment is invalid"));
        }
        Ok(())
    }

    fn prepare_session_directories(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<PreparedSessionDirectories> {
        let session_directory = self.session_directory(session_id)?;
        let attachment_directory = session_directory.join("attachments");
        let private_directory = session_directory.join("private");
        prepare_private_directory(&session_directory).map_err(|source| {
            internal_error("session data directory could not be prepared", source)
        })?;
        prepare_private_directory(&attachment_directory).map_err(|source| {
            internal_error("session attachment directory could not be prepared", source)
        })?;
        prepare_private_directory(&private_directory).map_err(|source| {
            internal_error("session private directory could not be prepared", source)
        })?;
        Ok(PreparedSessionDirectories {
            session_directory,
            attachment_directory,
            private_directory,
        })
    }
}

pub(super) fn remove_created_session_directories(paths: &PreparedSessionDirectories) {
    let _ = fs::remove_dir(&paths.attachment_directory);
    let _ = fs::remove_dir(&paths.private_directory);
    let _ = fs::remove_dir(&paths.session_directory);
}

fn path_text(path: &Path) -> StorageResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_data("runtime path is not valid UTF-8"))
}
