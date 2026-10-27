//! 新会话首次发送跨文件系统与 SQLite 的窄原子物化操作。

use std::{collections::HashSet, fs, path::Path};

use agent_types::UserPart;
use assistant_protocol::{
    ConversationOwner, IdempotencyKey, InputId, SessionId, SessionTitleOrigin,
};
use assistant_runtime::{
    AcceptedInput, NewStoredSession, NewStoredSessionMaterialization, StoreError, StoreErrorKind,
    StoredAttachment, StoredAttachmentState, StoredConversationState, StoredInput,
    StoredInputState, StoredSession, StoredSessionLifecycle, StoredSessionMaterialization,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, attachment_io, body_path, database_write_error,
    input_state::{input_origin_value, validate_new_input_activation},
    internal_error, invalid_data,
    mode::{agent_variant_value, approval_mode_value, reasoning_effort_value},
    session_resources::remove_created_session_directories,
    sync_directory, to_i64,
};

impl StorageEngine {
    /// 只跟随原接纳身份；原 Input 被清空或改写后不能退回当前队首。
    fn first_materialized_input(&mut self, session_id: &SessionId) -> StorageResult<StoredInput> {
        let raw: Option<String> = self.connection.query_row(
            "SELECT materialization_input_id FROM sessions WHERE session_id=?1 AND materialization_key IS NOT NULL",
            [session_id.as_str()], |row| row.get(0),
        ).map_err(|source| internal_error("materialized Input identity could not be read",source))?;
        let raw = raw.ok_or_else(|| {
            super::conflict("original materialization Input identity is unavailable")
        })?;
        let id = InputId::new(raw).map_err(|source| {
            super::invalid_data_with_source("materialized Input ID is invalid", source)
        })?;
        self.load_input_for_id(session_id, &id)?
            .ok_or_else(|| super::conflict("original materialization Input was removed"))
    }

    /// 队列载荷仍留 SQL；已消费原文只按当前 owner 的稳定消息位置读取一条。
    /// 不吞掉损坏/待提交尾帧并伪装成内容不同，也不加载完整会话正文作幂等比较。
    fn original_input_message(
        &mut self,
        input: &StoredInput,
    ) -> StorageResult<agent_types::UserMessage> {
        if let Some(message) = &input.queued_message {
            return Ok(message.clone());
        }
        let owner = ConversationOwner::MainSession {
            session_id: input.session_id.clone(),
        };
        let location = self
            .locate_conversation_message(assistant_runtime::ConversationMessageLocationRequest {
                owner: owner.clone(),
                message_id: input.user_message_id.clone(),
            })?
            .ok_or_else(|| invalid_data("materialized User Message is missing"))?;
        let body =
            self.load_conversation_raw_window(assistant_runtime::ConversationRawWindowRequest {
                owner,
                generation: location.generation,
                start: usize::try_from(location.message_ordinal).map_err(|source| {
                    internal_error("materialized message ordinal exceeds range", source)
                })?,
                limit: 1,
            })?;
        match body.conversation.messages.into_iter().next() {
            Some(agent_types::ConversationMessage::User(message))
                if message.id == input.user_message_id =>
            {
                Ok(message)
            }
            _ => Err(invalid_data(
                "materialized input message is not its original User",
            )),
        }
    }

    /// 启动时只清理由 Store 管理、但没有任何 SQLite 权威记录的物化遗留文件。
    pub(super) fn recover_materialization_orphans(&mut self) -> StorageResult<()> {
        let session_ids = {
            let mut statement = self
                .connection
                .prepare("SELECT session_id FROM sessions")
                .map_err(|source| {
                    internal_error("session identities could not be queried", source)
                })?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| internal_error("session identities could not be read", source))?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|source| {
                    internal_error("session identity row could not be read", source)
                })?
        };
        for entry in fs::read_dir(&self.sessions_directory)
            .map_err(|source| internal_error("session directory could not be scanned", source))?
        {
            let entry = entry.map_err(|source| {
                internal_error("session directory entry could not be read", source)
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if entry
                .file_type()
                .map_err(|source| {
                    internal_error("session directory entry could not be inspected", source)
                })?
                .is_dir()
                && assistant_protocol::SessionId::new(name.clone()).is_ok()
                && !session_ids.contains(&name)
            {
                fs::remove_dir_all(entry.path()).map_err(|source| {
                    internal_error(
                        "orphaned materialization session could not be removed",
                        source,
                    )
                })?;
            }
        }

        let referenced_blobs = {
            let mut statement = self
                .connection
                .prepare("SELECT relative_path FROM attachment_blobs")
                .map_err(|source| {
                    internal_error("attachment blob paths could not be queried", source)
                })?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| {
                    internal_error("attachment blob paths could not be read", source)
                })?
                .collect::<Result<HashSet<_>, _>>()
                .map_err(|source| {
                    internal_error("attachment blob path row could not be read", source)
                })?
        };
        let sha_directory = self.blobs_directory.join("sha256");
        if sha_directory.is_dir() {
            for bucket in fs::read_dir(&sha_directory).map_err(|source| {
                internal_error("attachment blob directory could not be scanned", source)
            })? {
                let bucket = bucket.map_err(|source| {
                    internal_error("attachment blob bucket could not be read", source)
                })?;
                if !bucket
                    .file_type()
                    .map_err(|source| {
                        internal_error("attachment blob bucket could not be inspected", source)
                    })?
                    .is_dir()
                {
                    continue;
                }
                for blob in fs::read_dir(bucket.path()).map_err(|source| {
                    internal_error("attachment blob bucket could not be scanned", source)
                })? {
                    let blob = blob.map_err(|source| {
                        internal_error("attachment blob entry could not be read", source)
                    })?;
                    if !blob
                        .file_type()
                        .map_err(|source| {
                            internal_error("attachment blob entry could not be inspected", source)
                        })?
                        .is_file()
                    {
                        continue;
                    }
                    let relative = blob
                        .path()
                        .strip_prefix(self.blobs_directory.parent().expect("data directory"))
                        .ok()
                        .and_then(Path::to_str)
                        .map(str::to_owned);
                    if relative.is_some_and(|path| !referenced_blobs.contains(&path)) {
                        fs::remove_file(blob.path()).map_err(|source| {
                            internal_error("orphaned attachment blob could not be removed", source)
                        })?;
                    }
                }
                let _ = fs::remove_dir(bucket.path());
            }
        }
        for entry in fs::read_dir(&self.upload_staging_directory).map_err(|source| {
            internal_error("upload staging directory could not be scanned", source)
        })? {
            let entry = entry.map_err(|source| {
                internal_error("upload staging entry could not be read", source)
            })?;
            if entry
                .file_type()
                .map_err(|source| {
                    internal_error("upload staging entry could not be inspected", source)
                })?
                .is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("part")
            {
                fs::remove_file(entry.path()).map_err(|source| {
                    internal_error("orphaned upload staging file could not be removed", source)
                })?;
            }
        }
        Ok(())
    }

    /// 文件准备完成后只用一次 SQLite 事务公开 Session/Input；失败时移除本次私有目录。
    pub(super) fn materialize_session(
        &mut self,
        materialization: NewStoredSessionMaterialization,
    ) -> StorageResult<StoredSessionMaterialization> {
        let key = materialization
            .session
            .materialization_key
            .as_ref()
            .ok_or_else(|| invalid_input("materialization key is required"))?;
        if let Some(existing) = self.get_session_materialization(key)? {
            let mut input = self.first_materialized_input(&existing.session.session_id)?;
            let persisted_message = self.original_input_message(&input)?;
            let selections = if input.state == StoredInputState::Committed {
                let facts = self
                    .load_conversation_history(ConversationOwner::MainSession {
                        session_id: input.session_id.clone(),
                    })?
                    .facts;
                input.skill_activation = facts
                    .skill_activations
                    .into_iter()
                    .find(|activation| activation.input_id.as_ref() == Some(&input.input_id));
                facts.mcp_selections
            } else {
                self.load_mcp_input_selections_scoped(Some(&input.session_id))?
            };
            let selection = selections
                .iter()
                .find(|selection| selection.input_id.as_ref() == Some(&input.input_id));
            cleanup_staging(&materialization);
            let selection_matches = selection
                .map(|selection| (&selection.server_key, selection.display_name.as_str()))
                == materialization
                    .input
                    .mcp_selection
                    .as_ref()
                    .map(|selection| (&selection.server_key, selection.display_name.as_str()));
            if !selection_matches
                || !materialization_semantically_matches(
                    &existing.session,
                    &existing.attachments,
                    &input,
                    Some(&persisted_message),
                    &materialization,
                )
            {
                return Err(super::conflict(
                    "materialization key was reused with different content",
                ));
            }
            return Ok(existing);
        }

        validate_materialization(&materialization)?;
        let session_paths = self.prepare_new_session_directories(&materialization.session)?;
        let body = body_path(&session_paths.session_directory, 1);
        if let Err(error) = super::conversation::create_empty(
            &body,
            &assistant_protocol::ConversationOwner::MainSession {
                session_id: materialization.session.session_id.clone(),
            },
            1,
        ) {
            cleanup_failed_session(&session_paths);
            return Err(error);
        }

        let data_directory = self
            .blobs_directory
            .parent()
            .expect("blobs directory is inside data directory");
        let mut prepared_attachments = Vec::with_capacity(materialization.attachments.len());
        let mut created_blobs = Vec::new();
        let prepared = (|| -> StorageResult<()> {
            for upload in &materialization.attachments {
                let staging = Path::new(&upload.staging_path);
                attachment_io::validate_original_name(&upload.original_name)?;
                attachment_io::validate_blob_hash(&upload.blob_hash)?;
                attachment_io::validate_staging_path(
                    &self.upload_staging_directory,
                    staging,
                    upload.size_bytes,
                )?;
                attachment_io::validate_staging_hash(
                    staging,
                    &upload.original_name,
                    &upload.blob_hash,
                )?;
                let (relative_blob, created) = attachment_io::ensure_blob(
                    data_directory,
                    staging,
                    &upload.original_name,
                    &upload.blob_hash,
                    upload.size_bytes,
                )?;
                if created {
                    created_blobs.push(data_directory.join(&relative_blob));
                }
                let view = attachment_io::stable_view_path(
                    &session_paths.attachment_directory,
                    &upload.attachment_id,
                    &upload.original_name,
                );
                attachment_io::ensure_stable_view(&view, &upload.blob_hash, &upload.original_name)?;
                let agent_readable_path = path_text(&view)?;
                prepared_attachments.push((upload, relative_blob, agent_readable_path));
            }
            sync_directory(&session_paths.session_directory)
        })();
        if let Err(error) = prepared {
            let _ = fs::remove_file(&body);
            cleanup_failed_session(&session_paths);
            remove_created_blobs(&created_blobs);
            cleanup_staging(&materialization);
            return Err(error);
        }

        let prompt_json = serde_json::to_string(&materialization.session.system_prompt)
            .map_err(|source| internal_error("system prompt could not be encoded", source))?;
        let skill_catalog_json = "{}";
        let message_json = serde_json::to_string(&materialization.input.message)
            .map_err(|source| internal_error("queued user message could not be encoded", source))?;
        let skill_activation_json = materialization
            .input
            .skill_activation
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|source| {
                internal_error("input skill activation could not be encoded", source)
            })?;
        let goal_reply_route_json = materialization
            .input
            .goal_binding
            .as_ref()
            .and_then(|binding| binding.reply_route.as_ref())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|source| internal_error("Goal reply route could not be encoded", source))?;

        let persisted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("materialization transaction could not be started", source)
                })?;
            insert_session(
                &transaction,
                &materialization.session,
                &prompt_json,
                skill_catalog_json,
            )?;
            let bound = transaction.execute(
                "UPDATE sessions SET materialization_input_id=?1 WHERE session_id=?2 AND materialization_key=?3 AND materialization_input_id IS NULL",
                params![materialization.input.input_id.as_str(),materialization.session.session_id.as_str(),key.as_str()],
            ).map_err(|source| database_write_error("original materialization identity could not be bound",source))?;
            if bound != 1 {
                return Err(super::conflict(
                    "materialization identity changed before acceptance",
                ));
            }
            Self::insert_session_resources(&transaction, &materialization.session)?;
            transaction
                .execute(
                    "INSERT INTO session_usage (session_id, backfilled, updated_at_ms)
                     VALUES (?1, 1, ?2)",
                    params![
                        materialization.session.session_id.as_str(),
                        materialization.session.created_at_ms
                    ],
                )
                .map_err(|source| {
                    database_write_error("session usage could not be initialized", source)
                })?;
            for (upload, relative_blob, agent_readable_path) in &prepared_attachments {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO attachment_blobs (
                            blob_hash, size_bytes, relative_path, media_type, created_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            upload.blob_hash,
                            to_i64(upload.size_bytes, "attachment size is too large")?,
                            path_text(relative_blob)?,
                            upload.media_type,
                            upload.created_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error(
                            "attachment blob metadata could not be created",
                            source,
                        )
                    })?;
                transaction
                    .execute(
                        "INSERT INTO attachments (
                            attachment_id, session_id, blob_hash, original_name,
                            agent_readable_path, state, created_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, 'ready', ?6)",
                        params![
                            upload.attachment_id.as_str(),
                            upload.session_id.as_str(),
                            upload.blob_hash,
                            upload.original_name,
                            agent_readable_path,
                            upload.created_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("attachment metadata could not be created", source)
                    })?;
            }
            if let Some(goal) = materialization.input.new_goal.as_ref() {
                super::goal::insert_new_goal(&transaction, goal)?;
            }
            transaction
                .execute(
                    "INSERT INTO inputs (
                        priority_order, input_id, session_id, idempotency_key, user_message_id,
                        state, queued_message_json, accepted_at_ms, agent_variant, origin,
                        goal_id, goal_generation, goal_turn, goal_reply_route_json,
                        skill_activation_json, cross_session_json, channel_source_json, agent_shell_target
                     ) VALUES (0, ?1, ?2, NULL, ?3, 'queued', ?4, ?5, ?6, ?7,
                               ?8, ?9, ?10, ?11, ?12, NULL, ?13, ?14)",
                    params![
                        materialization.input.input_id.as_str(),
                        materialization.input.session_id.as_str(),
                        materialization.input.message.id.as_str(),
                        message_json,
                        materialization.input.accepted_at_ms,
                        agent_variant_value(materialization.input.agent_variant),
                        input_origin_value(materialization.input.origin),
                        materialization
                            .input
                            .goal_binding
                            .as_ref()
                            .map(|binding| binding.goal_id.as_str()),
                        materialization
                            .input
                            .goal_binding
                            .as_ref()
                            .map(|binding| i64::try_from(binding.generation))
                            .transpose()
                            .map_err(|source| internal_error(
                                "Goal generation exceeds storage range",
                                source
                            ))?,
                        materialization
                            .input
                            .goal_binding
                            .as_ref()
                            .map(|binding| i64::from(binding.turn)),
                        goal_reply_route_json,
                        skill_activation_json,
                        serde_json::to_string(&materialization.input.channel_source).map_err(
                            |source| internal_error(
                                "input channel source could not be encoded",
                                source
                            )
                        )?,
                        materialization.input.agent_shell_target.map(super::mode::shell_kind_value),
                    ],
                )
                .map_err(|source| database_write_error("input could not be accepted", source))?;
            if let Some(selection) = materialization.input.mcp_selection.as_ref() {
                transaction
                    .execute(
                        "INSERT INTO mcp_input_selections (
                            selection_id, session_id, input_id, message_id,
                            server_key, display_name, created_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            selection.selection_id.as_str(),
                            selection.session_id.as_str(),
                            selection.input_id.as_ref().map(InputId::as_str),
                            selection.message_id.as_str(),
                            selection.server_key.as_str(),
                            selection.display_name.as_str(),
                            selection.created_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error(
                            "materialized MCP selection could not be accepted",
                            source,
                        )
                    })?;
            }
            if let Some(activation) = materialization.input.skill_activation.as_ref() {
                super::skill::insert_skill_activation(&transaction, activation)?;
            }
            transaction.commit().map_err(|source| {
                database_write_error("materialization transaction could not be committed", source)
            })
        })();
        if let Err(error) = persisted {
            let _ = fs::remove_file(&body);
            cleanup_failed_session(&session_paths);
            remove_created_blobs(&created_blobs);
            cleanup_staging(&materialization);
            return Err(error);
        }
        sync_directory(&self.sessions_directory)?;

        let session = stored_session(materialization.session);
        let attachments = prepared_attachments
            .into_iter()
            .map(|(upload, _, agent_readable_path)| StoredAttachment {
                attachment_id: upload.attachment_id.clone(),
                session_id: upload.session_id.clone(),
                original_name: upload.original_name.clone(),
                blob_hash: upload.blob_hash.clone(),
                size_bytes: upload.size_bytes,
                media_type: upload.media_type.clone(),
                agent_readable_path,
                state: StoredAttachmentState::Ready,
                created_at_ms: upload.created_at_ms,
            })
            .collect::<Vec<_>>();
        let goal = materialization.input.new_goal.clone();
        let accepted = accepted_input(materialization.input);
        let owner = ConversationOwner::MainSession {
            session_id: session.session_id.clone(),
        };
        let _ = self.initialize_recall_owner(&owner, 1, session.created_at_ms);
        Ok(StoredSessionMaterialization {
            goal,
            session,
            attachments,
            accepted,
        })
    }
    /// 只读定位现有唯一 key 的可靠事实；不准备目录、更新行或重放已执行输入。
    pub(super) fn get_session_materialization(
        &mut self,
        key: &IdempotencyKey,
    ) -> StorageResult<Option<StoredSessionMaterialization>> {
        let session_id = self
            .connection
            .query_row(
                "SELECT session_id FROM sessions WHERE materialization_key = ?1",
                [key.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("materialization key could not be queried", source))?
            .map(|id| {
                SessionId::new(id).map_err(|_| invalid_data("materialized session id is invalid"))
            })
            .transpose()?;
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let session = self
            .load_sessions_scoped(Some(&session_id))?
            .into_iter()
            .next()
            .ok_or_else(|| invalid_data("materialized session is missing"))?;
        let input = self.first_materialized_input(&session_id)?;
        let accepted = self.existing_input_acceptance(input.clone())?;
        let attachments = self.load_attachments_scoped(Some(&session_id))?;
        let persisted_message = self.original_input_message(&input)?;
        Ok(Some(StoredSessionMaterialization {
            goal: self
                .load_all_goals_scoped(Some(&session_id))?
                .into_iter()
                .next(),
            session,
            attachments: original_attachments(attachments, Some(&persisted_message)),
            accepted,
        }))
    }
}

fn validate_materialization(value: &NewStoredSessionMaterialization) -> StorageResult<()> {
    super::filesystem::validate_session_component(&value.session.session_id)?;
    if value.session.role != assistant_runtime::SessionRole::Standard
        || value.input.session_id != value.session.session_id
        || value.input.origin != assistant_runtime::InputOrigin::User
        || value.input.cross_session.is_some()
        || value.input.resumed_goal.is_some()
        || value.input.idempotency_key.is_some()
    {
        return Err(invalid_input("materialization shape is invalid"));
    }
    assistant_runtime::validate_input_message_with_channel_source(
        value.input.origin,
        value.input.goal_binding.as_ref(),
        value.input.cross_session.as_ref(),
        value.input.channel_source.as_ref(),
        &value.input.message,
    )
    .map_err(|_| invalid_input("materialized input message is invalid"))?;
    validate_new_input_activation(&value.input)?;
    if let Some(selection) = value.input.mcp_selection.as_ref()
        && (selection.session_id != value.input.session_id
            || selection.input_id.as_ref() != Some(&value.input.input_id)
            || selection.message_id != value.input.message.id
            || selection.display_name.trim().is_empty()
            || selection.display_name.len() > 128)
    {
        return Err(invalid_input("materialized MCP selection is inconsistent"));
    }
    if value.input.goal_binding.is_some() != value.input.new_goal.is_some() {
        return Err(invalid_input(
            "materialized Goal binding does not match Goal creation",
        ));
    }
    if let Some(goal) = value.input.new_goal.as_ref() {
        let binding = value
            .input
            .goal_binding
            .as_ref()
            .ok_or_else(|| invalid_input("materialized Goal has no binding"))?;
        if goal.session_id != value.session.session_id
            || goal.goal_id != binding.goal_id
            || goal.generation != binding.generation
            || goal.turn != binding.turn
            || goal.objective.source_message_id != value.input.message.id
        {
            return Err(invalid_input("materialized Goal is inconsistent"));
        }
    }
    let mut attachment_ids = HashSet::new();
    let mut attachment_hashes = HashSet::new();
    for upload in &value.attachments {
        super::filesystem::validate_attachment_component(&upload.attachment_id)?;
        if upload.session_id != value.session.session_id
            || !attachment_ids.insert(upload.attachment_id.as_str())
            || !attachment_hashes.insert(upload.blob_hash.as_str())
        {
            return Err(invalid_input("materialized attachment session is invalid"));
        }
    }
    Ok(())
}

fn insert_session(
    transaction: &rusqlite::Transaction<'_>,
    session: &NewStoredSession,
    prompt_json: &str,
    skill_catalog_json: &str,
) -> StorageResult<()> {
    transaction
        .execute(
            "INSERT INTO sessions (
                session_id, title, model_id, reasoning_effort, system_prompt_json,
                skill_catalog_json, current_variant, approval_mode, role, lifecycle,
                body_generation, message_count, created_at_ms, updated_at_ms, archived_at_ms,
                is_pinned, title_origin, materialization_key, automatic_title_pending, model_provider_instance_id, agent_shell_kind, agent_shell_environment_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'standard', 'active',
                       1, 0, ?9, ?9, NULL, 0, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                session.session_id.as_str(),
                session.title,
                session.model_selection.as_ref().map(|selection| selection.model_id.as_str()),
                session.reasoning_effort.map(reasoning_effort_value),
                prompt_json,
                skill_catalog_json,
                agent_variant_value(session.current_variant),
                approval_mode_value(session.approval_mode),
                session.created_at_ms,
                match session.title_origin {
                    SessionTitleOrigin::Generated => "generated",
                    SessionTitleOrigin::User => "user",
                },
                session.materialization_key.as_ref().map(|key| key.as_str()),
                i64::from(session.automatic_title_pending),
                        session.model_selection.as_ref().map(|selection| selection.provider_instance_id.as_str()),
                session.agent_shell_kind.map(super::mode::shell_kind_value),
super::agent_shell::encode_environment(session.agent_shell_environment.as_ref())?,
            ],
        )
        .map_err(|source| database_write_error("session could not be materialized", source))?;
    Ok(())
}

fn accepted_input(input: assistant_runtime::NewStoredInput) -> AcceptedInput {
    let stored = StoredInput {
        agent_shell_target: input.agent_shell_target,
        queue_order: 0,
        input_id: input.input_id.clone(),
        session_id: input.session_id.clone(),
        idempotency_key: input.idempotency_key,
        agent_variant: input.agent_variant,
        origin: input.origin,
        goal_binding: input.goal_binding,
        cross_session: input.cross_session,
        channel_source: input.channel_source,
        skill_activation: input.skill_activation,
        user_message_id: input.message.id.clone(),
        state: StoredInputState::Queued,
        queued_message: Some(input.message),
        accepted_at_ms: input.accepted_at_ms,
    };
    AcceptedInput::Control {
        input: Box::new(stored),
        run: None,
        is_duplicate: false,
    }
}

fn stored_session(session: NewStoredSession) -> StoredSession {
    StoredSession {
        context_management_mode: None,
        server_history_key: None,
        custom_agent: session.custom_agent,
        agent_shell_kind: session.agent_shell_kind,
        agent_shell_environment: session.agent_shell_environment.clone(),
        session_id: session.session_id,
        title: session.title,
        title_origin: session.title_origin,
        model_selection: session.model_selection,
        reasoning_effort: session.reasoning_effort,
        system_prompt: session.system_prompt,

        environment: session.environment,
        lifecycle: StoredSessionLifecycle::Active,
        current_variant: session.current_variant,
        approval_mode: session.approval_mode,
        role: session.role,
        materialization_key: session.materialization_key,
        automatic_title_pending: session.automatic_title_pending,
        proxy: None,
        pc_output_hosting: None,
        body_generation: 1,
        message_count: 0,
        created_at_ms: session.created_at_ms,
        updated_at_ms: session.created_at_ms,
        archived_at_ms: None,
        is_pinned: false,
        conversation_state: StoredConversationState::Available,
    }
}

fn materialization_semantically_matches(
    existing: &StoredSession,
    attachments: &[StoredAttachment],
    input: &StoredInput,
    persisted_message: Option<&agent_types::UserMessage>,
    candidate: &NewStoredSessionMaterialization,
) -> bool {
    let mut existing_files = attachments
        .iter()
        .map(|file| {
            (
                &file.original_name,
                &file.blob_hash,
                file.size_bytes,
                &file.media_type,
            )
        })
        .collect::<Vec<_>>();
    let mut candidate_files = candidate
        .attachments
        .iter()
        .map(|file| {
            (
                &file.original_name,
                &file.blob_hash,
                file.size_bytes,
                &file.media_type,
            )
        })
        .collect::<Vec<_>>();
    existing_files.sort_unstable();
    candidate_files.sort_unstable();
    existing.title == candidate.session.title
        && existing
            .custom_agent
            .as_ref()
            .map(|agent| (&agent.source, &agent.config.name))
            == candidate
                .session
                .custom_agent
                .as_ref()
                .map(|agent| (&agent.source, &agent.config.name))
        && existing.model_selection == candidate.session.model_selection
        && existing.reasoning_effort == candidate.session.reasoning_effort
        && existing.environment.workspace_id == candidate.session.environment.workspace_id
        && (existing.environment.workspace_id.is_none()
            || existing.environment.working_directory
                == candidate.session.environment.working_directory)
        && existing.environment.additional_workspace_directories
            == candidate
                .session
                .environment
                .additional_workspace_directories
        && existing.current_variant == candidate.session.current_variant
        && existing.approval_mode == candidate.session.approval_mode
        && existing_files == candidate_files
        && input.agent_variant == candidate.input.agent_variant
        && input.goal_binding.is_some() == candidate.input.goal_binding.is_some()
        && input.skill_activation.as_ref().map(|value| &value.name)
            == candidate
                .input
                .skill_activation
                .as_ref()
                .map(|value| &value.name)
        && normalized_message(persisted_message)
            == normalized_message(Some(&candidate.input.message))
}

fn normalized_message(message: Option<&agent_types::UserMessage>) -> Option<serde_json::Value> {
    let mut message = message?.clone();
    message
        .parts
        .retain(|part| !matches!(part, UserPart::InternalContext(_)));
    let mut value = serde_json::to_value(message).ok()?;
    remove_generated_message_fields(&mut value);
    Some(value)
}

fn remove_generated_message_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("id");
            map.remove("quote_id");
            map.remove("readable_path");
            for value in map.values_mut() {
                remove_generated_message_fields(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                remove_generated_message_fields(value);
            }
        }
        _ => {}
    }
}

fn original_attachments(
    mut attachments: Vec<StoredAttachment>,
    message: Option<&agent_types::UserMessage>,
) -> Vec<StoredAttachment> {
    let mut ordered = Vec::new();
    if let Some(message) = message {
        for file in message
            .parts
            .iter()
            .filter_map(|part| match part {
                UserPart::FileReferences(files) => Some(&files.files),
                _ => None,
            })
            .flatten()
        {
            if let Some(index) = attachments
                .iter()
                .position(|attachment| attachment.agent_readable_path == file.readable_path)
            {
                ordered.push(attachments.remove(index));
            }
        }
    }
    ordered
}

fn cleanup_staging(materialization: &NewStoredSessionMaterialization) {
    for upload in &materialization.attachments {
        match fs::remove_file(&upload.staging_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
}

fn cleanup_failed_session(paths: &super::session_resources::PreparedSessionDirectories) {
    remove_created_session_directories(paths);
    if paths.session_directory.exists() {
        let _ = fs::remove_dir_all(&paths.session_directory);
    }
}

fn remove_created_blobs(paths: &[std::path::PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn path_text(path: &Path) -> StorageResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_input("runtime path is not valid UTF-8"))
}

fn invalid_input(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::InvalidInput, message)
}
