//! Session Fork 与永久删除的跨 SQLite/JSONL/Attachment 介质提交边界。

use std::{collections::BTreeMap, fs, path::Path};

use agent_types::{ConversationMessage, ConversationSnapshot, UserPart};
use assistant_protocol::{ConversationOwner, DeleteSessionImpact, SessionId};
use assistant_runtime::{
    SessionDeletion, SessionFork, StoreError, StoreErrorKind, StoredAttachment,
    StoredAttachmentState, StoredConversationState, StoredSession, StoredSessionFork,
    StoredSessionLifecycle,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, attachment_io, body_path, conflict, conversation,
    database_write_error, internal_error, invalid_data,
    mode::{agent_variant_value, approval_mode_value, reasoning_effort_value},
    session_resources::remove_created_session_directories,
    sync_directory, to_i64,
};

impl StorageEngine {
    pub(super) fn fork_session(&mut self, fork: SessionFork) -> StorageResult<StoredSessionFork> {
        super::filesystem::validate_session_component(&fork.source_session_id)?;
        super::filesystem::validate_session_component(&fork.session.session_id)?;
        fork.conversation
            .validate_tool_exchange_pairs()
            .map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    StoreErrorKind::InvalidInput,
                    "fork conversation splits a tool exchange",
                    source,
                )
            })?;
        let source_generation = self
            .connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id = ?1",
                [fork.source_session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| {
                internal_error("fork source generation could not be queried", source)
            })?
            .ok_or_else(|| conflict("fork source session does not exist"))?;
        if u64::try_from(source_generation).ok() != Some(fork.source_generation) {
            return Err(conflict("fork source generation changed"));
        }
        let paths = self.prepare_new_session_directories(&fork.session)?;
        let prepared = (|| -> StorageResult<_> {
            let source_attachments = self
                .load_attachments()?
                .into_iter()
                .filter(|attachment| attachment.session_id == fork.source_session_id)
                .map(|attachment| (attachment.attachment_id.clone(), attachment))
                .collect::<BTreeMap<_, _>>();
            let mut rewrites = BTreeMap::new();
            let mut attachments = Vec::with_capacity(fork.attachments.len());
            for reference in &fork.attachments {
                super::filesystem::validate_attachment_component(&reference.attachment_id)?;
                let source = source_attachments
                    .get(&reference.source_attachment_id)
                    .ok_or_else(|| conflict("fork attachment does not belong to source session"))?;
                let view = attachment_io::stable_view_path(
                    &paths.attachment_directory,
                    &reference.attachment_id,
                    &source.original_name,
                );
                attachment_io::ensure_stable_view(&view, &source.blob_hash, &source.original_name)?;
                let readable_path = path_text(&view)?;
                rewrites.insert(source.agent_readable_path.clone(), readable_path.clone());
                attachments.push(StoredAttachment {
                    attachment_id: reference.attachment_id.clone(),
                    session_id: fork.session.session_id.clone(),
                    original_name: source.original_name.clone(),
                    blob_hash: source.blob_hash.clone(),
                    size_bytes: source.size_bytes,
                    media_type: source.media_type.clone(),
                    agent_readable_path: readable_path,
                    state: source.state,
                    created_at_ms: fork.session.created_at_ms,
                });
            }
            let source_tool_images = self
                .session_directory(&fork.source_session_id)?
                .join("tool-images");
            for reference in &fork.tool_images {
                crate::image::copy_tool_image(
                    &source_tool_images,
                    &paths.tool_image_directory,
                    reference,
                )
                .map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::ResourceUnavailable,
                        "fork tool image could not be copied",
                        source,
                    )
                })?;
            }
            let mut forked_conversation = fork.conversation;
            rewrite_file_reference_paths(&mut forked_conversation, &rewrites)?;
            let payload = conversation::encode_messages(&forked_conversation.messages)?;
            conversation::decode(std::io::BufReader::new(payload.as_slice()))?;
            let body = body_path(&paths.session_directory, 1);
            conversation::write_replacement(&body, &payload)?;
            sync_directory(&paths.session_directory)?;
            let prompt_json =
                serde_json::to_string(&fork.session.system_prompt).map_err(|source| {
                    internal_error("fork system prompt could not be encoded", source)
                })?;
            let message_count =
                u64::try_from(forked_conversation.messages.len()).map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::InvalidInput,
                        "fork conversation is too large",
                        source,
                    )
                })?;
            Ok((forked_conversation, attachments, prompt_json, message_count))
        })();
        let (forked_conversation, attachments, prompt_json, message_count) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                remove_created_session_directories(&paths);
                return Err(error);
            }
        };
        // Session 目录及其中的 Conversation/Attachment 视图必须先持久化，再提交
        // SQLite 中对这些文件的引用。提交成功后不再执行可能导致“已提交但返回失败”的步骤。
        sync_directory(&self.sessions_directory)?;
        let persisted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("fork transaction could not begin", source)
                })?;
            transaction
                .execute(
                    "INSERT INTO sessions (
                        session_id, title, model_key, reasoning_effort, system_prompt_json, current_variant,
                        approval_mode, lifecycle, body_generation, message_count, created_at_ms,
                        updated_at_ms, archived_at_ms, is_pinned, title_origin
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', 1, ?8, ?9, ?9, NULL, 0, ?10)",
                    params![
                        fork.session.session_id.as_str(),
                        fork.session.title,
                        fork.session.model_key.as_str(),
                        fork.session.reasoning_effort.map(reasoning_effort_value),
                        prompt_json,
                        agent_variant_value(fork.session.current_variant),
                        approval_mode_value(fork.session.approval_mode),
                        to_i64(message_count, "fork message count exceeds SQLite range")?,
                        fork.session.created_at_ms,
                        match fork.session.title_origin {
                            assistant_protocol::SessionTitleOrigin::Generated => "generated",
                            assistant_protocol::SessionTitleOrigin::User => "user",
                        },
                    ],
                )
                .map_err(|source| {
                    database_write_error("fork session could not be created", source)
                })?;
            Self::insert_session_resources(&transaction, &fork.session)?;
            transaction
                .execute(
                    "INSERT INTO session_usage (session_id, backfilled, updated_at_ms)
                     VALUES (?1, 1, ?2)",
                    params![fork.session.session_id.as_str(), fork.session.created_at_ms],
                )
                .map_err(|source| {
                    database_write_error("fork session usage could not be initialized", source)
                })?;
            for attachment in &attachments {
                transaction
                    .execute(
                        "INSERT INTO attachments (
                            attachment_id, session_id, blob_hash, original_name,
                            agent_readable_path, state, created_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            attachment.attachment_id.as_str(),
                            attachment.session_id.as_str(),
                            attachment.blob_hash,
                            attachment.original_name,
                            attachment.agent_readable_path,
                            match attachment.state {
                                StoredAttachmentState::Ready => "ready",
                                StoredAttachmentState::Unavailable => "unavailable",
                            },
                            attachment.created_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error(
                            "fork attachment reference could not be created",
                            source,
                        )
                    })?;
            }
            transaction
                .commit()
                .map_err(|source| database_write_error("fork transaction could not commit", source))
        })();
        if let Err(error) = persisted {
            remove_created_session_directories(&paths);
            return Err(error);
        }
        let stored = StoredSessionFork {
            session: StoredSession {
                session_id: fork.session.session_id,
                title: fork.session.title,
                model_key: fork.session.model_key,
                reasoning_effort: fork.session.reasoning_effort,
                system_prompt: fork.session.system_prompt,
                environment: fork.session.environment,
                lifecycle: StoredSessionLifecycle::Active,
                current_variant: fork.session.current_variant,
                approval_mode: fork.session.approval_mode,
                body_generation: 1,
                message_count,
                created_at_ms: fork.session.created_at_ms,
                updated_at_ms: fork.session.created_at_ms,
                archived_at_ms: None,
                is_pinned: false,
                title_origin: fork.session.title_origin,
                conversation_state: StoredConversationState::Available,
            },
            conversation: forked_conversation,
            attachments,
        };
        self.mark_recall_owner_dirty_now(
            &ConversationOwner::MainSession {
                session_id: stored.session.session_id.clone(),
            },
            1,
        );
        Ok(stored)
    }

    pub(super) fn inspect_session_deletion(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<DeleteSessionImpact> {
        super::filesystem::validate_session_component(session_id)?;
        self.connection
            .query_row(
                "SELECT message_count,
                        (SELECT COUNT(*) FROM runs WHERE session_id = ?1),
                        (SELECT COUNT(*) FROM child_tasks WHERE session_id = ?1),
                        (SELECT COUNT(*) FROM attachments WHERE session_id = ?1)
                 FROM sessions WHERE session_id = ?1",
                [session_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("delete impact could not be queried", source))?
            .map(|row| {
                Ok(DeleteSessionImpact {
                    message_count: non_negative(row.0, "delete message count is invalid")?,
                    run_count: non_negative(row.1, "delete run count is invalid")?,
                    child_task_count: non_negative(row.2, "delete child count is invalid")?,
                    attachment_count: non_negative(row.3, "delete attachment count is invalid")?,
                })
            })
            .transpose()?
            .ok_or_else(|| conflict("delete session does not exist"))
    }

    pub(super) fn delete_session(&mut self, deletion: SessionDeletion) -> StorageResult<()> {
        super::filesystem::validate_session_component(&deletion.session_id)?;
        self.ensure_session_idle_for_transfer(&deletion.session_id)?;
        if self.inspect_session_deletion(&deletion.session_id)? != deletion.expected_impact {
            return Err(conflict("delete session impact changed"));
        }
        let source = self.session_directory(&deletion.session_id)?;
        let staged = self
            .deletion_staging_directory
            .join(format!("{}.{}", deletion.session_id, deletion.operation_id));
        if staged.exists() {
            return Err(conflict("delete staging path already exists"));
        }
        self.conversation_indexes.remove_under(&source);
        fs::rename(&source, &staged).map_err(|source| {
            internal_error("session directory could not enter delete staging", source)
        })?;
        sync_directory(&self.sessions_directory)?;
        sync_directory(&self.deletion_staging_directory)?;

        let deleted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("delete transaction could not begin", source)
                })?;
            let changed = transaction
                .execute(
                    "DELETE FROM sessions WHERE session_id = ?1",
                    [deletion.session_id.as_str()],
                )
                .map_err(|source| database_write_error("session could not be deleted", source))?;
            if changed != 1 {
                return Err(conflict("delete session does not exist"));
            }
            transaction.commit().map_err(|source| {
                database_write_error("delete transaction could not commit", source)
            })
        })();
        if let Err(error) = deleted {
            if fs::rename(&staged, &source).is_err() {
                return Err(invalid_data(
                    "failed delete could not restore session directory",
                ));
            }
            let _ = sync_directory(&self.sessions_directory);
            let _ = sync_directory(&self.deletion_staging_directory);
            return Err(error);
        }
        self.unavailable_sessions
            .remove(deletion.session_id.as_str());
        if fs::remove_dir_all(&staged).is_ok() {
            let _ = sync_directory(&self.deletion_staging_directory);
        }
        Ok(())
    }

    pub(super) fn recover_session_deletions(&mut self) -> StorageResult<()> {
        for entry in fs::read_dir(&self.deletion_staging_directory).map_err(|source| {
            internal_error("delete staging directory could not be read", source)
        })? {
            let entry = entry.map_err(|source| {
                internal_error("delete staging entry could not be read", source)
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid_data("delete staging entry name is invalid"))?;
            let (session, _) = name
                .split_once('.')
                .ok_or_else(|| invalid_data("delete staging entry name is invalid"))?;
            let session_id = SessionId::new(session.to_owned())
                .map_err(|_| invalid_data("delete staging session id is invalid"))?;
            super::filesystem::validate_session_component(&session_id)?;
            let exists = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
                    [session_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|source| {
                    internal_error("delete recovery session could not be queried", source)
                })?
                == 1;
            let staged = entry.path();
            if exists {
                let target = self.sessions_directory.join(session_id.as_str());
                if target.exists() {
                    return Err(invalid_data(
                        "delete recovery found duplicate session directories",
                    ));
                }
                fs::rename(&staged, &target).map_err(|source| {
                    internal_error(
                        "delete recovery could not restore session directory",
                        source,
                    )
                })?;
            } else {
                fs::remove_dir_all(&staged).map_err(|source| {
                    internal_error("committed delete staging could not be removed", source)
                })?;
            }
        }
        sync_directory(&self.sessions_directory)?;
        sync_directory(&self.deletion_staging_directory)
    }

    fn ensure_session_idle_for_transfer(&self, session_id: &SessionId) -> StorageResult<()> {
        let busy = self
            .connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM inputs WHERE session_id = ?1 AND state = 'queued'
                    UNION ALL
                    SELECT 1 FROM runs WHERE session_id = ?1
                        AND status IN ('accepted', 'running', 'cancelling')
                    UNION ALL
                    SELECT 1 FROM child_tasks WHERE session_id = ?1
                        AND status IN ('accepted', 'running')
                    UNION ALL
                    SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?1
                    UNION ALL
                    SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?1
                 )",
                [session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| {
                internal_error("session transfer state could not be queried", source)
            })?;
        if busy == 1 {
            return Err(conflict("session is not idle"));
        }
        Ok(())
    }
}

fn rewrite_file_reference_paths(
    conversation: &mut ConversationSnapshot,
    rewrites: &BTreeMap<String, String>,
) -> StorageResult<()> {
    for message in &mut conversation.messages {
        let ConversationMessage::User(user) = message else {
            continue;
        };
        for part in &mut user.parts {
            let UserPart::FileReferences(references) = part else {
                continue;
            };
            for file in &mut references.files {
                file.readable_path =
                    rewrites.get(&file.readable_path).cloned().ok_or_else(|| {
                        conflict("fork conversation references an unmapped attachment")
                    })?;
            }
        }
    }
    Ok(())
}

fn path_text(path: &Path) -> StorageResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| StoreError::new(StoreErrorKind::InvalidInput, "path must be UTF-8"))
}

fn non_negative(value: i64, message: &'static str) -> StorageResult<u64> {
    u64::try_from(value).map_err(|_| invalid_data(message))
}
