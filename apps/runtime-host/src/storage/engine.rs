//! SQLite connection、Session 投影与 Conversation 加载入口。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use agent_model::SystemPromptSnapshot;
use assistant_protocol::{ModelKey, SessionId};
use assistant_runtime::{
    NewStoredSession, RecoveredRuntime, StoredConversationState, StoredSession,
    StoredSessionLifecycle,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    BLOBS_DIRECTORY, BUSY_TIMEOUT, DATA_DIRECTORY, DATABASE_FILE, SESSIONS_DIRECTORY,
    STAGING_DIRECTORY, StorageResult, WORKSPACES_DIRECTORY, body_path, conflict, conversation,
    create_new_private_file, database_write_error, internal_error, invalid_data,
    invalid_data_with_source,
    mode::{agent_variant_value, approval_mode_value, parse_agent_variant, parse_approval_mode},
    non_negative_u64, positive_u64, schema,
    session_resources::remove_created_session_directories,
    sync_directory,
};
use crate::config_source::prepare_private_directory;

/// 单一阻塞 worker 内部的具体存储引擎。
pub(super) struct StorageEngine {
    pub(super) runtime_home: PathBuf,
    pub(super) sessions_directory: PathBuf,
    pub(super) workspaces_directory: PathBuf,
    pub(super) blobs_directory: PathBuf,
    pub(super) upload_staging_directory: PathBuf,
    pub(super) connection: Connection,
    unavailable_sessions: HashSet<String>,
    pub(super) unavailable_child_tasks: HashSet<String>,
}

impl StorageEngine {
    pub(super) fn open(runtime_home: &Path) -> StorageResult<Self> {
        let data_directory = runtime_home.join(DATA_DIRECTORY);
        let sessions_directory = data_directory.join(SESSIONS_DIRECTORY);
        let workspaces_directory = data_directory.join(WORKSPACES_DIRECTORY);
        let blobs_directory = data_directory.join(BLOBS_DIRECTORY);
        let upload_staging_directory = data_directory.join(STAGING_DIRECTORY);
        prepare_private_directory(&data_directory).map_err(|source| {
            internal_error("runtime data directory could not be prepared", source)
        })?;
        prepare_private_directory(&sessions_directory).map_err(|source| {
            internal_error("runtime sessions directory could not be prepared", source)
        })?;
        prepare_private_directory(&workspaces_directory).map_err(|source| {
            internal_error("runtime workspaces directory could not be prepared", source)
        })?;
        prepare_private_directory(&blobs_directory).map_err(|source| {
            internal_error("runtime blobs directory could not be prepared", source)
        })?;
        prepare_private_directory(&upload_staging_directory).map_err(|source| {
            internal_error(
                "runtime upload staging directory could not be prepared",
                source,
            )
        })?;

        let database_path = data_directory.join(DATABASE_FILE);
        super::filesystem::prepare_private_file(&database_path)?;
        let mut connection = Connection::open(&database_path)
            .map_err(|source| internal_error("runtime database could not be opened", source))?;
        connection.busy_timeout(BUSY_TIMEOUT).map_err(|source| {
            internal_error("runtime database timeout could not be set", source)
        })?;
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|source| {
                internal_error("runtime database journal mode could not be set", source)
            })?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|source| {
                internal_error("runtime database durability could not be set", source)
            })?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|source| {
                internal_error("runtime database foreign keys could not be enabled", source)
            })?;
        schema::initialize(&mut connection)?;

        let mut engine = Self {
            runtime_home: runtime_home.to_path_buf(),
            sessions_directory,
            workspaces_directory,
            blobs_directory,
            upload_staging_directory,
            connection,
            unavailable_sessions: HashSet::new(),
            unavailable_child_tasks: HashSet::new(),
        };
        engine.repair_session_resources()?;
        engine.recover_attachments()?;
        Ok(engine)
    }

    pub(super) fn load_runtime(&mut self) -> StorageResult<RecoveredRuntime> {
        let mut unavailable = self.recover_body_appends()?;
        self.unavailable_child_tasks = self.recover_child_storage()?;
        self.interrupt_nonterminal_child_tasks()?;
        // parent delegate result 必须在 child 工具交换和 child 终态修复之后重建。
        unavailable.extend(self.recover_pending_tool_exchanges()?);
        self.unavailable_sessions = unavailable;
        self.interrupt_nonterminal_runs()?;
        Ok(RecoveredRuntime {
            workspaces: self.load_all_workspaces()?,
            attachments: self.load_attachments()?,
            sessions: self.load_sessions()?,
            inputs: self.load_inputs()?,
            runs: self.load_runs()?,
            child_tasks: self.load_child_tasks()?,
        })
    }

    pub(super) fn create_session(
        &mut self,
        session: NewStoredSession,
    ) -> StorageResult<StoredSession> {
        super::filesystem::validate_session_component(&session.session_id)?;
        let paths = self.prepare_new_session_directories(&session)?;
        let body_path = body_path(&paths.session_directory, 1);
        create_new_private_file(&body_path)?;
        sync_directory(&paths.session_directory)?;

        let prompt_json = serde_json::to_string(&session.system_prompt)
            .map_err(|source| internal_error("system prompt could not be encoded", source))?;
        let persisted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("session transaction could not be started", source)
                })?;
            transaction
                .execute(
                    "INSERT INTO sessions (
                        session_id, title, model_key, system_prompt_json, current_variant,
                        approval_mode, lifecycle, body_generation, message_count, created_at_ms,
                        updated_at_ms, archived_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', 1, 0, ?7, ?7, NULL)",
                    params![
                        session.session_id.as_str(),
                        session.title,
                        session.model_key.as_str(),
                        prompt_json,
                        agent_variant_value(session.current_variant),
                        approval_mode_value(session.approval_mode),
                        session.created_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("session could not be created in runtime storage", source)
                })?;
            Self::insert_session_resources(&transaction, &session)?;
            transaction.commit().map_err(|source| {
                database_write_error("session transaction could not be committed", source)
            })?;
            Ok(())
        })();
        if let Err(error) = persisted {
            let _ = fs::remove_file(&body_path);
            remove_created_session_directories(&paths);
            return Err(error);
        }
        sync_directory(&self.sessions_directory)?;

        Ok(StoredSession {
            session_id: session.session_id,
            title: session.title,
            model_key: session.model_key,
            system_prompt: session.system_prompt,
            environment: session.environment,
            lifecycle: StoredSessionLifecycle::Active,
            current_variant: session.current_variant,
            approval_mode: session.approval_mode,
            body_generation: 1,
            message_count: 0,
            created_at_ms: session.created_at_ms,
            updated_at_ms: session.created_at_ms,
            archived_at_ms: None,
            conversation_state: StoredConversationState::Available,
        })
    }

    pub(super) fn load_conversation(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<agent_types::ConversationSnapshot> {
        super::filesystem::validate_session_component(session_id)?;
        if self.unavailable_sessions.contains(session_id.as_str()) {
            return Err(invalid_data("session conversation is unavailable"));
        }
        let generation = self.session_generation(session_id)?;
        conversation::read(&body_path(&self.session_directory(session_id)?, generation))
    }

    fn load_sessions(&self) -> StorageResult<Vec<StoredSession>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT session_id, title, model_key, system_prompt_json, current_variant,
                        approval_mode, lifecycle, body_generation, message_count, created_at_ms,
                        updated_at_ms, archived_at_ms
                 FROM sessions
                 ORDER BY created_at_ms, session_id",
            )
            .map_err(|source| internal_error("runtime sessions could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            })
            .map_err(|source| internal_error("runtime sessions could not be read", source))?;

        let mut sessions = Vec::new();
        for row in rows {
            let (
                session_id,
                title,
                model_key,
                prompt_json,
                current_variant,
                approval_mode,
                lifecycle,
                body_generation,
                message_count,
                created_at_ms,
                updated_at_ms,
                archived_at_ms,
            ) = row.map_err(|source| {
                internal_error("runtime session row could not be read", source)
            })?;
            let parsed_session_id = SessionId::new(session_id.clone()).map_err(|source| {
                invalid_data_with_source("stored session id is invalid", source)
            })?;
            super::filesystem::validate_session_component(&parsed_session_id).map_err(|_| {
                invalid_data("stored session id cannot be used as a path component")
            })?;
            let parsed_model_key = ModelKey::new(model_key).map_err(|source| {
                invalid_data_with_source("stored model key is invalid", source)
            })?;
            let system_prompt: SystemPromptSnapshot =
                serde_json::from_str(&prompt_json).map_err(|source| {
                    invalid_data_with_source("stored system prompt is invalid", source)
                })?;
            let lifecycle = match lifecycle.as_str() {
                "active" => StoredSessionLifecycle::Active,
                "archived" => StoredSessionLifecycle::Archived,
                _ => return Err(invalid_data("stored session lifecycle is invalid")),
            };
            sessions.push(StoredSession {
                session_id: parsed_session_id.clone(),
                title,
                model_key: parsed_model_key,
                system_prompt,
                environment: self.load_session_environment(&parsed_session_id)?,
                lifecycle,
                current_variant: parse_agent_variant(&current_variant)?,
                approval_mode: parse_approval_mode(&approval_mode)?,
                body_generation: positive_u64(
                    body_generation,
                    "stored body generation is invalid",
                )?,
                message_count: non_negative_u64(message_count, "stored message count is invalid")?,
                created_at_ms,
                updated_at_ms,
                archived_at_ms,
                conversation_state: if self.unavailable_sessions.contains(&session_id) {
                    StoredConversationState::Unavailable
                } else {
                    StoredConversationState::Available
                },
            });
        }
        Ok(sessions)
    }

    pub(super) fn session_generation(&self, session_id: &SessionId) -> StorageResult<u64> {
        let generation = self
            .connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id = ?1",
                [session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| internal_error("session generation could not be queried", source))?
            .ok_or_else(|| conflict("session does not exist in runtime storage"))?;
        positive_u64(generation, "stored body generation is invalid")
    }

    pub(super) fn session_directory(&self, session_id: &SessionId) -> StorageResult<PathBuf> {
        super::filesystem::validate_session_component(session_id)?;
        Ok(self.sessions_directory.join(session_id.as_str()))
    }
}
