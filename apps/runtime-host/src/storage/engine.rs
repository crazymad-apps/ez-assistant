//! SQLite connection、Session 投影与 Conversation 加载入口。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use agent_model::SystemPromptSnapshot;
use assistant_protocol::{ConversationOwner, ModelKey, SessionId, SessionTitleOrigin};
use assistant_runtime::{
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationWindowRequest,
    NewStoredSession, RecoveredRuntime, StoredConversationMessageLocation,
    StoredConversationRawWindow, StoredConversationState, StoredConversationWindow, StoredSession,
    StoredSessionLifecycle,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    BLOBS_DIRECTORY, BUSY_TIMEOUT, DATA_DIRECTORY, DATABASE_FILE, DELETION_STAGING_DIRECTORY,
    SESSIONS_DIRECTORY, STAGING_DIRECTORY, StorageResult, WORKSPACES_DIRECTORY, body_path,
    conflict, conversation, create_new_private_file, database_write_error, internal_error,
    invalid_data, invalid_data_with_source,
    mode::{
        agent_variant_value, approval_mode_value, parse_agent_variant, parse_approval_mode,
        parse_reasoning_effort, reasoning_effort_value,
    },
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
    pub(super) deletion_staging_directory: PathBuf,
    pub(super) connection: Connection,
    pub(super) unavailable_sessions: HashSet<String>,
    pub(super) unavailable_child_tasks: HashSet<String>,
    pub(super) conversation_indexes: conversation::ConversationIndexCache,
    pub(super) recall_index_available: bool,
}

impl StorageEngine {
    pub(super) fn open(runtime_home: &Path) -> StorageResult<Self> {
        let data_directory = runtime_home.join(DATA_DIRECTORY);
        let sessions_directory = data_directory.join(SESSIONS_DIRECTORY);
        let workspaces_directory = data_directory.join(WORKSPACES_DIRECTORY);
        let blobs_directory = data_directory.join(BLOBS_DIRECTORY);
        let upload_staging_directory = data_directory.join(STAGING_DIRECTORY);
        let deletion_staging_directory = data_directory.join(DELETION_STAGING_DIRECTORY);
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
        prepare_private_directory(&deletion_staging_directory).map_err(|source| {
            internal_error(
                "runtime deletion staging directory could not be prepared",
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
        let recall_index_available = schema::initialize_recall_fts(&mut connection);

        let mut engine = Self {
            runtime_home: runtime_home.to_path_buf(),
            sessions_directory,
            workspaces_directory,
            blobs_directory,
            upload_staging_directory,
            deletion_staging_directory,
            connection,
            unavailable_sessions: HashSet::new(),
            unavailable_child_tasks: HashSet::new(),
            conversation_indexes: conversation::ConversationIndexCache::default(),
            recall_index_available,
        };
        engine.recover_session_deletions()?;
        engine.repair_workspace_resources()?;
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
        let tool_image_diagnostics = self.recover_tool_images()?;
        self.unavailable_sessions.extend(tool_image_diagnostics);
        self.interrupt_nonterminal_runs()?;
        self.pause_running_goals_for_recovery()?;
        self.backfill_session_usage()?;
        Ok(RecoveredRuntime {
            workspaces: self.load_all_workspaces()?,
            attachments: self.load_attachments()?,
            sessions: self.load_sessions()?,
            inputs: self.load_inputs()?,
            runs: self.load_runs()?,
            child_tasks: self.load_child_tasks()?,
            work_plans: self.load_all_work_plans()?,
            goals: self.load_all_goals()?,
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
                        session_id, title, model_key, reasoning_effort, system_prompt_json, current_variant,
                        approval_mode, lifecycle, body_generation, message_count, created_at_ms,
                        updated_at_ms, archived_at_ms, is_pinned, title_origin
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 1, 0, ?8, ?8, NULL, 0, ?9)",
                    params![
                        session.session_id.as_str(),
                        session.title,
                        session.model_key.as_str(),
                        session.reasoning_effort.map(reasoning_effort_value),
                        prompt_json,
                        agent_variant_value(session.current_variant),
                        approval_mode_value(session.approval_mode),
                        session.created_at_ms,
                        match session.title_origin {
                            SessionTitleOrigin::Generated => "generated",
                            SessionTitleOrigin::User => "user",
                        },
                    ],
                )
                .map_err(|source| {
                    database_write_error("session could not be created in runtime storage", source)
                })?;
            Self::insert_session_resources(&transaction, &session)?;
            transaction
                .execute(
                    "INSERT INTO session_usage (session_id, backfilled, updated_at_ms)
                     VALUES (?1, 1, ?2)",
                    params![session.session_id.as_str(), session.created_at_ms],
                )
                .map_err(|source| {
                    database_write_error("session usage could not be initialized", source)
                })?;
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

        let recall_owner = assistant_protocol::ConversationOwner::MainSession {
            session_id: session.session_id.clone(),
        };
        let _ = self.initialize_recall_owner(&recall_owner, 1, session.created_at_ms);

        Ok(StoredSession {
            session_id: session.session_id,
            title: session.title,
            model_key: session.model_key,
            reasoning_effort: session.reasoning_effort,
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
            is_pinned: false,
            title_origin: session.title_origin,
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

    pub(super) fn load_conversation_window(
        &mut self,
        request: ConversationWindowRequest,
    ) -> StorageResult<StoredConversationWindow> {
        let path = match &request.owner {
            ConversationOwner::MainSession { session_id } => {
                super::filesystem::validate_session_component(session_id)?;
                if self.unavailable_sessions.contains(session_id.as_str()) {
                    return Err(invalid_data("session conversation is unavailable"));
                }
                if self.session_generation(session_id)? != request.generation {
                    return Err(conflict("conversation generation changed"));
                }
                body_path(&self.session_directory(session_id)?, request.generation)
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                super::filesystem::validate_session_component(session_id)?;
                super::filesystem::validate_child_task_component(child_task_id)?;
                if self
                    .unavailable_child_tasks
                    .contains(child_task_id.as_str())
                {
                    return Err(invalid_data("child conversation is unavailable"));
                }
                if self.child_generation(session_id, child_task_id)? != request.generation {
                    return Err(conflict("conversation generation changed"));
                }
                self.child_body(session_id, child_task_id, request.generation)?
            }
        };
        self.conversation_indexes
            .read_window(&path, request.generation, request.end, request.limit)
    }

    pub(super) fn load_conversation_raw_window(
        &mut self,
        request: ConversationRawWindowRequest,
    ) -> StorageResult<StoredConversationRawWindow> {
        let path = match &request.owner {
            ConversationOwner::MainSession { session_id } => {
                super::filesystem::validate_session_component(session_id)?;
                if self.unavailable_sessions.contains(session_id.as_str()) {
                    return Err(invalid_data("session conversation is unavailable"));
                }
                if self.session_generation(session_id)? != request.generation {
                    return Err(conflict("conversation generation changed"));
                }
                body_path(&self.session_directory(session_id)?, request.generation)
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                super::filesystem::validate_session_component(session_id)?;
                super::filesystem::validate_child_task_component(child_task_id)?;
                if self
                    .unavailable_child_tasks
                    .contains(child_task_id.as_str())
                {
                    return Err(invalid_data("child conversation is unavailable"));
                }
                if self.child_generation(session_id, child_task_id)? != request.generation {
                    return Err(conflict("conversation generation changed"));
                }
                self.child_body(session_id, child_task_id, request.generation)?
            }
        };
        let (conversation, end, total) =
            self.conversation_indexes
                .read_raw_window(&path, request.start, request.limit)?;
        let start = request.start.min(total);
        Ok(StoredConversationRawWindow {
            generation: request.generation,
            start,
            end,
            total,
            conversation,
        })
    }

    pub(super) fn locate_conversation_message(
        &mut self,
        request: ConversationMessageLocationRequest,
    ) -> StorageResult<Option<StoredConversationMessageLocation>> {
        let (generation, path) = match &request.owner {
            ConversationOwner::MainSession { session_id } => {
                super::filesystem::validate_session_component(session_id)?;
                if self.unavailable_sessions.contains(session_id.as_str()) {
                    return Err(invalid_data("session conversation is unavailable"));
                }
                let generation = self.session_generation(session_id)?;
                (
                    generation,
                    body_path(&self.session_directory(session_id)?, generation),
                )
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                super::filesystem::validate_session_component(session_id)?;
                super::filesystem::validate_child_task_component(child_task_id)?;
                if self
                    .unavailable_child_tasks
                    .contains(child_task_id.as_str())
                {
                    return Err(invalid_data("child conversation is unavailable"));
                }
                let generation = self.child_generation(session_id, child_task_id)?;
                (
                    generation,
                    self.child_body(session_id, child_task_id, generation)?,
                )
            }
        };
        self.conversation_indexes
            .locate_message(&path, &request.message_id)?
            .map(|(ordinal, display_ordinal)| {
                Ok(StoredConversationMessageLocation {
                    generation,
                    message_ordinal: u64::try_from(ordinal).map_err(|source| {
                        internal_error("conversation ordinal exceeds storage range", source)
                    })?,
                    display_ordinal: display_ordinal.map(u64::try_from).transpose().map_err(
                        |source| {
                            internal_error(
                                "conversation display ordinal exceeds storage range",
                                source,
                            )
                        },
                    )?,
                })
            })
            .transpose()
    }

    fn load_sessions(&self) -> StorageResult<Vec<StoredSession>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT session_id, title, model_key, reasoning_effort, system_prompt_json, current_variant,
                        approval_mode, lifecycle, body_generation, message_count, created_at_ms,
                        COALESCE((SELECT MAX(runs.finished_at_ms) FROM runs
                                  WHERE runs.session_id = sessions.session_id), created_at_ms),
                        archived_at_ms, is_pinned, title_origin
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
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, String>(14)?,
                ))
            })
            .map_err(|source| internal_error("runtime sessions could not be read", source))?;

        let mut sessions = Vec::new();
        for row in rows {
            let (
                session_id,
                title,
                model_key,
                reasoning_effort,
                prompt_json,
                current_variant,
                approval_mode,
                lifecycle,
                body_generation,
                message_count,
                created_at_ms,
                updated_at_ms,
                archived_at_ms,
                is_pinned,
                title_origin,
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
                reasoning_effort: parse_reasoning_effort(reasoning_effort)?,
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
                is_pinned: match is_pinned {
                    0 => false,
                    1 => true,
                    _ => return Err(invalid_data("stored session pinned state is invalid")),
                },
                title_origin: match title_origin.as_str() {
                    "generated" => SessionTitleOrigin::Generated,
                    "user" => SessionTitleOrigin::User,
                    _ => return Err(invalid_data("stored session title origin is invalid")),
                },
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
