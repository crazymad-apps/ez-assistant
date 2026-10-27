//! 跨 SQLite 与 Conversation 文件的 staged append 和 generation 切换。

mod child_directory;

use std::{collections::HashSet, fs};

use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{ChildTaskId, ConversationOwner, RunId, SessionId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult,
    append_effect::{
        AppendPurpose, ConversationStorageTarget, apply_purpose, decode_purpose, encode_purpose,
    },
    body_path, conflict, conversation, database_write_error, internal_error, invalid_data,
    non_negative_u64, sync_directory, to_i64,
};

pub(super) struct AppendRequest {
    pub operation_id: String,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub messages: Vec<ConversationMessage>,
    pub message_step: Option<u32>,
    pub created_at_ms: i64,
}

pub(super) struct ChildAppendRequest {
    pub operation_id: String,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub messages: Vec<ConversationMessage>,
    pub message_step: Option<u32>,
    pub created_at_ms: i64,
    pub purpose: AppendPurpose,
}

// 文件验证完成、尚未发布的提交；单 owner 和 child／主目录组合使用同一 SQL 完成算法。
struct PreparedAppend {
    staged: StagedAppend,
    table: AppendTable,
    batch: conversation::ConversationDocument,
    framed: bool,
    recall_owner: ConversationOwner,
    base_message_ordinal: u64,
}

pub(super) struct ReplacementPlan {
    pub session_id: SessionId,
    pub previous_generation: u64,
    pub new_generation: u64,
    pub message_count: u64,
    pub changed_at_ms: i64,
}

pub(super) struct StagedAppend {
    pub(super) operation_id: String,
    pub(super) target: ConversationStorageTarget,
    pub(super) body_generation: u64,
    pub(super) base_byte_length: u64,
    pub(super) payload: Vec<u8>,
    pub(super) message_count_delta: u64,
    pub(super) message_step: Option<u32>,
    pub(super) created_at_ms: i64,
    pub(super) purpose: AppendPurpose,
}

#[derive(Clone, Copy)]
enum AppendTable {
    Session,
    ChildTask,
}

impl StorageEngine {
    pub(super) fn append_message_feedback(
        &mut self,
        session_id: SessionId,
        change: assistant_runtime::StoredHistoryFeedback,
    ) -> StorageResult<()> {
        let operation_id = format!(
            "message-feedback:{}:{}:{:?}",
            change.message_id, change.changed_at_ms, change.feedback
        );
        let at_ms = change.changed_at_ms;
        self.stage_target_append(
            operation_id.clone(),
            ConversationStorageTarget::SessionMetadata { session_id },
            Vec::new(),
            None,
            at_ms,
            AppendPurpose::MessageFeedback { change },
        )?;
        self.complete_staged_append(&operation_id)
    }
    pub(super) fn append_messages(&mut self, request: AppendRequest) -> StorageResult<()> {
        let operation_id = request.operation_id.clone();
        self.stage_append(request)?;
        self.complete_staged_append(&operation_id)
    }

    pub(super) fn stage_append(&mut self, request: AppendRequest) -> StorageResult<()> {
        self.stage_append_for(request, AppendPurpose::Messages)
    }

    pub(super) fn stage_append_for(
        &mut self,
        request: AppendRequest,
        purpose: AppendPurpose,
    ) -> StorageResult<()> {
        self.stage_target_append(
            request.operation_id,
            ConversationStorageTarget::Session {
                session_id: request.session_id,
                run_id: request.run_id,
            },
            request.messages,
            request.message_step,
            request.created_at_ms,
            purpose,
        )
    }

    pub(super) fn append_child_messages(
        &mut self,
        request: ChildAppendRequest,
    ) -> StorageResult<()> {
        let operation = request.operation_id.clone();
        self.stage_target_append(
            request.operation_id,
            ConversationStorageTarget::ChildTask {
                session_id: request.session_id,
                child_task_id: request.child_task_id,
            },
            request.messages,
            request.message_step,
            request.created_at_ms,
            request.purpose,
        )?;
        self.complete_target_append(&operation, AppendTable::ChildTask)
    }

    pub(super) fn stage_target_append(
        &mut self,
        operation_id: String,
        target: ConversationStorageTarget,
        messages: Vec<ConversationMessage>,
        message_step: Option<u32>,
        created_at_ms: i64,
        purpose: AppendPurpose,
    ) -> StorageResult<()> {
        if operation_id.trim().is_empty()
            || (messages.is_empty() && matches!(purpose, AppendPurpose::Messages))
        {
            return Err(assistant_runtime::StoreError::new(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "staged conversation append is invalid",
            ));
        }
        let generation = self.target_generation(&target)?;
        if let AppendPurpose::RunSettlement {
            proxy_report: Some(report),
            ..
        } = &purpose
        {
            self.ensure_new_input_identity(
                &report.session_id,
                &report.input_id,
                None,
                &report.message.id,
            )?;
        }
        let path = self.target_body_path(&target, generation)?;
        let owner = target.owner();
        self.ensure_no_pending_history(&owner)?;
        match &target {
            ConversationStorageTarget::Session { session_id, .. }
            | ConversationStorageTarget::SessionMetadata { session_id }
                if self.unavailable_sessions.contains(session_id.as_str()) =>
            {
                return Err(invalid_data("session conversation is unavailable"));
            }
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } if self.unavailable_child_tasks.contains(&(
                session_id.as_str().to_owned(),
                child_task_id.as_str().to_owned(),
            )) =>
            {
                return Err(invalid_data("child conversation is unavailable"));
            }
            _ => {}
        }
        let existing_history = self
            .conversation_indexes
            .validate_append(&path, &owner, generation, &messages)?;
        if existing_history.is_none() && !self.allow_legacy_history {
            return Err(invalid_data("conversation history header is missing"));
        }
        let payload = if existing_history.is_some() {
            conversation::encode_history_append(
                &operation_id,
                &messages,
                self.history_for_append(
                    &target,
                    &messages,
                    message_step,
                    &purpose,
                    created_at_ms,
                    existing_history.as_ref(),
                )?,
            )?
        } else {
            if messages.is_empty() {
                return Err(invalid_data("metadata append requires file history format"));
            }
            conversation::encode_messages(&messages)?
        };
        let base_byte_length = fs::metadata(&path)
            .map_err(|source| internal_error("conversation metadata could not be read", source))?
            .len();
        let message_count_delta = u64::try_from(messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidInput,
                "conversation message batch is too large",
                source,
            )
        })?;

        let kind = encode_purpose(&purpose)?;
        match &target {
            ConversationStorageTarget::Session { session_id, .. }
            | ConversationStorageTarget::SessionMetadata { session_id } => self.connection.execute(
                "INSERT INTO body_appends (
                    operation_id, session_id, run_id, body_generation, base_byte_length,
                    kind, payload, message_count_delta, message_step, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    operation_id,
                    session_id.as_str(),
                    match &target {
                        ConversationStorageTarget::Session { run_id, .. } => Some(run_id.as_str()),
                        _ => None,
                    },
                    to_i64(generation, "body generation exceeds SQLite range")?,
                    to_i64(base_byte_length, "conversation file exceeds SQLite range")?,
                    kind,
                    payload,
                    to_i64(message_count_delta, "message count exceeds SQLite range")?,
                    message_step,
                    created_at_ms,
                ],
            ),
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.connection.execute(
                "INSERT INTO child_body_appends (
                    operation_id, child_task_id, session_id, body_generation, base_byte_length,
                    kind, payload, message_count_delta, message_step, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    operation_id,
                    child_task_id.as_str(),
                    session_id.as_str(),
                    to_i64(generation, "body generation exceeds SQLite range")?,
                    to_i64(base_byte_length, "conversation file exceeds SQLite range")?,
                    kind,
                    payload,
                    to_i64(message_count_delta, "message count exceeds SQLite range")?,
                    message_step,
                    created_at_ms,
                ],
            ),
        }
        .map_err(|source| {
            database_write_error("conversation append could not be staged", source)
        })?;
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "split-stage append entrypoint is exercised by recovery tests"
        )
    )]
    pub(super) fn write_staged_append(&mut self, operation_id: &str) -> StorageResult<()> {
        self.write_target_append(operation_id, AppendTable::Session)
    }

    fn write_target_append(&mut self, operation_id: &str, table: AppendTable) -> StorageResult<()> {
        let staged = self.staged_append(operation_id, table)?;
        self.verify_staged_generation(&staged)?;
        let path = self.target_body_path(&staged.target, staged.body_generation)?;
        if !self.allow_legacy_history
            && !conversation::has_history_header(
                &path,
                &staged.target.owner(),
                staged.body_generation,
            )?
        {
            return Err(invalid_data("conversation history header is missing"));
        }
        self.conversation_indexes.retain_current_prefix(&path);
        conversation::reconcile_append(&path, staged.base_byte_length, &staged.payload)
    }

    fn prepare_target_append(
        &mut self,
        operation_id: &str,
        table: AppendTable,
    ) -> StorageResult<PreparedAppend> {
        let staged = self.staged_append(operation_id, table)?;
        self.verify_staged_generation(&staged)?;
        if staged.operation_id != operation_id {
            return Err(invalid_data("staged append identity is inconsistent"));
        }
        let path = self.target_body_path(&staged.target, staged.body_generation)?;
        let framed = conversation::has_history_header(
            &path,
            &staged.target.owner(),
            staged.body_generation,
        )?;
        if !framed && !self.allow_legacy_history {
            return Err(invalid_data("conversation history header is missing"));
        }
        let batch = conversation::decode_append(&staged.payload, framed, &staged.target.owner())?;
        let actual_delta = u64::try_from(batch.conversation.messages.len()).map_err(|source| {
            assistant_runtime::StoreError::with_source(
                assistant_runtime::StoreErrorKind::InvalidData,
                "staged append message count is invalid",
                source,
            )
        })?;
        if actual_delta != staged.message_count_delta {
            return Err(invalid_data(
                "staged append message count does not match payload",
            ));
        }
        if framed
            && matches!(
                &staged.target,
                ConversationStorageTarget::SessionMetadata { .. }
            )
        {
            let expected = self.history_for_append(
                &staged.target,
                &[],
                None,
                &staged.purpose,
                staged.created_at_ms,
                None,
            )?;
            if !batch.conversation.messages.is_empty() || batch.history.as_ref() != Some(&expected)
            {
                return Err(invalid_data(
                    "session metadata receipt does not match its frozen history",
                ));
            }
        }

        let (recall_owner, base_message_ordinal) = match &staged.target {
            ConversationStorageTarget::Session { session_id, .. }
            | ConversationStorageTarget::SessionMetadata { session_id } => {
                let count = self
                    .connection
                    .query_row(
                        "SELECT message_count FROM sessions
                     WHERE session_id = ?1 AND body_generation = ?2",
                        params![
                            session_id.as_str(),
                            to_i64(
                                staged.body_generation,
                                "body generation exceeds SQLite range"
                            )?
                        ],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|source| {
                        internal_error("conversation message count could not be read", source)
                    })?;
                (
                    ConversationOwner::MainSession {
                        session_id: session_id.clone(),
                    },
                    non_negative_u64(count, "conversation message count is invalid")?,
                )
            }
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } => {
                let count = self
                    .connection
                    .query_row(
                        "SELECT message_count FROM child_tasks
                     WHERE child_task_id = ?1 AND session_id = ?2 AND body_generation = ?3",
                        params![
                            child_task_id.as_str(),
                            session_id.as_str(),
                            to_i64(
                                staged.body_generation,
                                "body generation exceeds SQLite range"
                            )?
                        ],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|source| {
                        internal_error("child conversation message count could not be read", source)
                    })?;
                (
                    ConversationOwner::ChildTask {
                        session_id: session_id.clone(),
                        child_task_id: child_task_id.clone(),
                    },
                    non_negative_u64(count, "child conversation message count is invalid")?,
                )
            }
        };

        Ok(PreparedAppend {
            staged,
            table,
            batch,
            framed,
            recall_owner,
            base_message_ordinal,
        })
    }

    fn finalize_prepared_appends(&mut self, appends: Vec<PreparedAppend>) -> StorageResult<()> {
        // 报告目标不是源 owner，源回执不能阻止其继续产生历史。最终 SQL 接纳前重查当前目标。
        // FeedbackInput 属于原 owner，由它自己的唯一回执阻止历史变化，不读未完成尾帧。
        for append in &appends {
            if let AppendPurpose::RunSettlement {
                proxy_report: Some(report),
                ..
            } = &append.staged.purpose
            {
                self.ensure_new_input_identity(
                    &report.session_id,
                    &report.input_id,
                    None,
                    &report.message.id,
                )?;
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation append could not begin finalization", source)
            })?;
        for append in &appends {
            let PreparedAppend {
                staged,
                table,
                batch,
                framed,
                recall_owner,
                ..
            } = append;
            let framed = *framed;
            let operation_id = staged.operation_id.as_str();
            let updated = match &staged.target {
                ConversationStorageTarget::Session { session_id, .. }
                | ConversationStorageTarget::SessionMetadata { session_id } => transaction.execute(
                    "UPDATE sessions
                 SET message_count = message_count + ?1
                 WHERE session_id = ?2 AND body_generation = ?3",
                    params![
                        to_i64(
                            staged.message_count_delta,
                            "message count exceeds SQLite range"
                        )?,
                        session_id.as_str(),
                        to_i64(
                            staged.body_generation,
                            "body generation exceeds SQLite range"
                        )?,
                    ],
                ),
                ConversationStorageTarget::ChildTask {
                    session_id,
                    child_task_id,
                } => transaction.execute(
                    "UPDATE child_tasks SET message_count = message_count + ?1
                 WHERE child_task_id = ?2 AND session_id = ?3 AND body_generation = ?4",
                    params![
                        to_i64(
                            staged.message_count_delta,
                            "message count exceeds SQLite range"
                        )?,
                        child_task_id.as_str(),
                        session_id.as_str(),
                        to_i64(
                            staged.body_generation,
                            "body generation exceeds SQLite range"
                        )?,
                    ],
                ),
            }
            .map_err(|source| {
                internal_error("conversation append metadata could not be updated", source)
            })?;
            if updated != 1 {
                return Err(invalid_data(
                    "staged append generation is no longer authoritative",
                ));
            }
            if !framed && let ConversationStorageTarget::Session { run_id, .. } = &staged.target {
                for message in &batch.conversation.messages {
                    transaction
                    .execute(
                        "INSERT INTO run_message_refs (run_id, message_id, step) VALUES (?1, ?2, ?3)",
                        params![
                            run_id.as_str(),
                            conversation::message_id(message).as_str(),
                            staged.message_step,
                        ],
                    )
                    .map_err(|source| {
                        internal_error("run message reference could not be recorded", source)
                    })?;
                }
            }
            super::usage::record_usage_messages(
                &transaction,
                recall_owner,
                &batch.conversation.messages,
                staged.created_at_ms,
            )?;
            apply_purpose(
                &transaction,
                &staged.purpose,
                &staged.target,
                staged.created_at_ms,
                self.allow_legacy_history,
            )?;
            let deleted = match table {
                AppendTable::Session => transaction.execute(
                    "DELETE FROM body_appends WHERE operation_id = ?1",
                    [operation_id],
                ),
                AppendTable::ChildTask => transaction.execute(
                    "DELETE FROM child_body_appends WHERE operation_id = ?1",
                    [operation_id],
                ),
            }
            .map_err(|source| internal_error("staged append could not be cleared", source))?;
            if deleted != 1 {
                return Err(invalid_data(
                    "staged append disappeared during finalization",
                ));
            }
        }
        // 所有原回执都经过精确删除核验后，才释放可能级联回执的执行控制。
        for append in &appends {
            let PreparedAppend {
                staged,
                batch,
                framed,
                ..
            } = append;
            let framed = *framed;
            if framed
                && !self.allow_legacy_history
                && matches!(&staged.purpose, AppendPurpose::RunSettlement { .. })
                && let ConversationStorageTarget::Session { session_id, run_id } = &staged.target
            {
                super::run_state::release_terminal_run_control(&transaction, session_id, run_id)?;
            }
            if framed
                && !self.allow_legacy_history
                && matches!(&staged.purpose, AppendPurpose::ToolExchange { .. })
                && let ConversationStorageTarget::Session { session_id, run_id } = &staged.target
            {
                super::tool_exchange::release_parent_child_controls(
                    &transaction,
                    session_id,
                    run_id,
                    &batch
                        .history
                        .as_ref()
                        .ok_or_else(|| invalid_data("parent child history is missing"))?
                        .child_tasks,
                )?;
            }
            if framed
                && !self.allow_legacy_history
                && matches!(&staged.purpose, AppendPurpose::ChildSettlement { .. })
                && let ConversationStorageTarget::ChildTask {
                    session_id,
                    child_task_id,
                } = &staged.target
            {
                let children = &batch
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid_data("settled child history is missing"))?
                    .child_tasks;
                let child = children
                    .iter()
                    .find(|child| &child.child_task_id == child_task_id)
                    .ok_or_else(|| invalid_data("settled child owner history is missing"))?;
                super::tool_exchange::release_parent_child_controls(
                    &transaction,
                    session_id,
                    &child.parent_run_id,
                    std::slice::from_ref(child),
                )?;
            }
        }
        transaction.commit().map_err(|source| {
            internal_error("conversation append could not be finalized", source)
        })?;
        for append in appends {
            let PreparedAppend {
                staged,
                batch,
                recall_owner,
                base_message_ordinal,
                ..
            } = append;
            // 先完成文件及 SQL，再推进派生位置；重启／失败路径仍以文件重建为准。
            if let Ok(path) = self.target_body_path(&staged.target, staged.body_generation) {
                self.conversation_indexes.record_committed_append(
                    &path,
                    staged.base_byte_length,
                    staged.payload.len(),
                    &staged.target.owner(),
                    staged.body_generation,
                    &batch,
                );
            }
            self.index_committed_recall_batch(
                &recall_owner,
                staged.body_generation,
                base_message_ordinal,
                staged.created_at_ms,
                &batch.conversation.messages,
            );
        }
        Ok(())
    }

    pub(super) fn complete_staged_append(&mut self, operation_id: &str) -> StorageResult<()> {
        self.complete_target_append(operation_id, AppendTable::Session)
    }

    fn complete_target_append(
        &mut self,
        operation_id: &str,
        table: AppendTable,
    ) -> StorageResult<()> {
        let staged = self.staged_append(operation_id, table)?;
        if !self.allow_legacy_history {
            if matches!(
                (&staged.target, &staged.purpose),
                (
                    ConversationStorageTarget::ChildTask { .. },
                    AppendPurpose::ChildSettlement {
                        parent_exchange_receipt: None,
                        ..
                    }
                )
            ) {
                return self.complete_standalone_child(staged);
            }
            if matches!(&staged.purpose, AppendPurpose::ChildDirectory { .. }) {
                return self.complete_child_directory(staged);
            }
        }
        drop(staged);
        self.write_target_append(operation_id, table)?;
        let prepared = self.prepare_target_append(operation_id, table)?;
        self.finalize_prepared_appends(vec![prepared])
    }

    pub(super) fn begin_replacement(
        &mut self,
        session_id: SessionId,
        snapshot: ConversationSnapshot,
        changed_at_ms: i64,
    ) -> StorageResult<ReplacementPlan> {
        // Round-trip through the same reader used after restart. This checks duplicate IDs, Tool
        // pairing and the exact bytes before a new generation can become authoritative.
        let owner = ConversationOwner::MainSession {
            session_id: session_id.clone(),
        };
        let history = self.history_for_replacement(&owner, &snapshot)?;
        let previous_generation = self.session_generation(&session_id)?;
        let session_directory = self.session_directory(&session_id)?;
        let mut new_generation = previous_generation
            .checked_add(1)
            .ok_or_else(|| conflict("conversation generation is exhausted"))?;
        while body_path(&session_directory, new_generation).exists() {
            new_generation = new_generation
                .checked_add(1)
                .ok_or_else(|| conflict("conversation generation is exhausted"))?;
        }
        let new_path = body_path(&session_directory, new_generation);
        conversation::write_generation(&new_path, &owner, new_generation, &snapshot, &history)?;
        sync_directory(&session_directory)?;

        Ok(ReplacementPlan {
            session_id,
            previous_generation,
            new_generation,
            message_count: u64::try_from(snapshot.messages.len()).map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    assistant_runtime::StoreErrorKind::InvalidInput,
                    "replacement conversation is too large",
                    source,
                )
            })?,
            changed_at_ms,
        })
    }

    pub(super) fn commit_replacement(&mut self, plan: &ReplacementPlan) -> StorageResult<()> {
        let session_directory = self.session_directory(&plan.session_id)?;
        let new_path = body_path(&session_directory, plan.new_generation);
        let replacement = self
            .read_owner_document(
                &new_path,
                &ConversationOwner::MainSession {
                    session_id: plan.session_id.clone(),
                },
                plan.new_generation,
            )?
            .conversation;

        let owner = ConversationOwner::MainSession {
            session_id: plan.session_id.clone(),
        };
        let new_usage =
            self.new_replacement_usage_messages(&owner, plan.previous_generation, &replacement)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation generation switch could not begin", source)
            })?;
        let updated = transaction
            .execute(
                "UPDATE sessions
                 SET body_generation = ?1, message_count = ?2
                 WHERE session_id = ?3 AND body_generation = ?4",
                params![
                    to_i64(plan.new_generation, "body generation exceeds SQLite range")?,
                    to_i64(plan.message_count, "message count exceeds SQLite range")?,
                    plan.session_id.as_str(),
                    to_i64(
                        plan.previous_generation,
                        "body generation exceeds SQLite range"
                    )?,
                ],
            )
            .map_err(|source| {
                internal_error("conversation generation could not be switched", source)
            })?;
        if updated != 1 {
            return Err(conflict(
                "conversation generation changed before replacement commit",
            ));
        }
        super::usage::record_usage_messages(
            &transaction,
            &ConversationOwner::MainSession {
                session_id: plan.session_id.clone(),
            },
            new_usage,
            plan.changed_at_ms,
        )?;
        super::usage::snapshot::replace_context(&transaction, &owner, &replacement)?;
        transaction.commit().map_err(|source| {
            internal_error(
                "conversation generation switch could not be committed",
                source,
            )
        })?;

        self.mark_recall_owner_dirty_now(
            &ConversationOwner::MainSession {
                session_id: plan.session_id.clone(),
            },
            plan.new_generation,
        );

        // 新 generation 已包含完整权威正文；删除失败只留下不可见孤立文件，不回滚已提交切换。
        let old_path = body_path(&session_directory, plan.previous_generation);
        if fs::remove_file(old_path).is_ok() {
            sync_directory(&session_directory)?;
        }
        Ok(())
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "staged operation count is a recovery test assertion"
        )
    )]
    pub(super) fn staged_append_count(&self) -> StorageResult<u64> {
        let count = self
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|source| internal_error("staged append count could not be read", source))?;
        non_negative_u64(count, "staged append count is invalid")
    }

    pub(super) fn recover_body_appends(&mut self) -> StorageResult<HashSet<String>> {
        self.recover_body_appends_scoped(None)
    }
    pub(super) fn recover_body_appends_scoped(
        &mut self,
        scope: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<HashSet<String>> {
        let pending = {
            let mut statement = self
                .connection
                .prepare("SELECT operation_id, session_id FROM body_appends WHERE (?1 IS NULL OR session_id = ?1) ORDER BY created_at_ms, operation_id")
                .map_err(|source| internal_error("staged appends could not be queried", source))?;
            let rows = statement
                .query_map([scope.map(assistant_protocol::SessionId::as_str)], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| internal_error("staged appends could not be read", source))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|source| internal_error("staged append row could not be read", source))?
        };

        let mut unavailable = HashSet::new();
        for (operation_id, session_id) in pending {
            if self.complete_staged_append(&operation_id).is_err() {
                unavailable.insert(session_id);
            }
        }
        Ok(unavailable)
    }

    pub(super) fn recover_child_body_appends(
        &mut self,
    ) -> StorageResult<HashSet<(String, String)>> {
        self.recover_child_body_appends_scoped(None)
    }
    pub(super) fn recover_child_body_appends_scoped(
        &mut self,
        scope: Option<&assistant_protocol::SessionId>,
    ) -> StorageResult<HashSet<(String, String)>> {
        let pending = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT operation_id, session_id, child_task_id FROM child_body_appends WHERE (?1 IS NULL OR session_id = ?1)
                     ORDER BY created_at_ms, operation_id",
                )
                .map_err(|source| {
                    internal_error("staged child appends could not be queried", source)
                })?;
            let rows = statement
                .query_map([scope.map(assistant_protocol::SessionId::as_str)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|source| {
                    internal_error("staged child appends could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error("staged child append row could not be read", source)
            })?
        };
        let mut unavailable = HashSet::new();
        for (operation_id, session_id, child_task_id) in pending {
            if self
                .complete_target_append(&operation_id, AppendTable::ChildTask)
                .is_err()
            {
                unavailable.insert((session_id, child_task_id));
            }
        }
        Ok(unavailable)
    }

    fn staged_append(&self, operation_id: &str, table: AppendTable) -> StorageResult<StagedAppend> {
        let query = match table {
            AppendTable::Session => {
                "SELECT operation_id, session_id, run_id, body_generation, base_byte_length,
                        kind, payload, message_count_delta, message_step, created_at_ms
                 FROM body_appends WHERE operation_id = ?1"
            }
            AppendTable::ChildTask => {
                "SELECT operation_id, child_task_id, session_id, body_generation,
                        base_byte_length, kind, payload, message_count_delta, message_step, created_at_ms
                 FROM child_body_appends WHERE operation_id = ?1"
            }
        };
        let row = self
            .connection
            .query_row(query, [operation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<u32>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .optional()
            .map_err(|source| internal_error("staged append could not be queried", source))?
            .ok_or_else(|| conflict("staged append does not exist"))?;
        let purpose = decode_purpose(&row.5)?;
        let target = match table {
            AppendTable::Session => {
                let session_id = SessionId::new(row.1).map_err(|source| {
                    super::invalid_data_with_source("staged append session id is invalid", source)
                })?;
                match row.2 {
                    Some(id) => ConversationStorageTarget::Session {
                        session_id,
                        run_id: RunId::new(id).map_err(|source| {
                            super::invalid_data_with_source(
                                "staged append run id is invalid",
                                source,
                            )
                        })?,
                    },
                    None => ConversationStorageTarget::SessionMetadata { session_id },
                }
            }
            AppendTable::ChildTask => ConversationStorageTarget::ChildTask {
                child_task_id: ChildTaskId::new(row.1).map_err(|source| {
                    super::invalid_data_with_source(
                        "staged append child task id is invalid",
                        source,
                    )
                })?,
                session_id: SessionId::new(
                    row.2
                        .ok_or_else(|| invalid_data("child append session identity is missing"))?,
                )
                .map_err(|source| {
                    super::invalid_data_with_source("staged append session id is invalid", source)
                })?,
            },
        };
        Ok(StagedAppend {
            operation_id: row.0,
            target,
            body_generation: super::positive_u64(row.3, "staged append generation is invalid")?,
            base_byte_length: non_negative_u64(row.4, "staged append base length is invalid")?,
            payload: row.6,
            message_count_delta: super::non_negative_u64(
                row.7,
                "staged append message count is invalid",
            )?,
            message_step: row.8,
            created_at_ms: row.9,
            purpose,
        })
    }

    fn verify_staged_generation(&self, staged: &StagedAppend) -> StorageResult<()> {
        if self.target_generation(&staged.target)? != staged.body_generation {
            return Err(invalid_data(
                "staged append targets a non-authoritative generation",
            ));
        }
        Ok(())
    }

    fn target_generation(&self, target: &ConversationStorageTarget) -> StorageResult<u64> {
        match target {
            ConversationStorageTarget::Session { session_id, .. }
            | ConversationStorageTarget::SessionMetadata { session_id } => {
                self.session_generation(session_id)
            }
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.child_generation(session_id, child_task_id),
        }
    }

    fn target_body_path(
        &self,
        target: &ConversationStorageTarget,
        generation: u64,
    ) -> StorageResult<std::path::PathBuf> {
        match target {
            ConversationStorageTarget::Session { session_id, .. }
            | ConversationStorageTarget::SessionMetadata { session_id } => {
                Ok(body_path(&self.session_directory(session_id)?, generation))
            }
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } => self.child_body(session_id, child_task_id, generation),
        }
    }
}
