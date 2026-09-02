//! Conversation Recall 的可重建 FTS5 派生索引。
//!
//! Conversation JSONL 与 SQLite 中的 Conversation 元数据始终是权威数据；本模块只维护
//! 可丢弃、可重建的搜索投影。权威写入完成后会尽力增量更新索引，失败时只把对应 owner
//! 标记为 dirty，不能反向使已经提交的 Conversation 失败。搜索时再按 owner 有界推进重建，
//! 因而 Runtime 启动不需要扫描全部历史正文。

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use agent_types::{AssistantPart, ConversationMessage, MessageId, UserPart};
use assistant_protocol::{ChildTaskId, ConversationOwner, SessionId};
use assistant_runtime::{
    ConversationSearchHit, ConversationSearchPage, ConversationSearchRequest,
    ConversationSearchScope, StoreError, StoreErrorKind,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{StorageEngine, StorageResult, body_path, internal_error, non_negative_u64, to_i64};

/// 单个 owner 在一次搜索中最多从权威 JSONL 读取的消息数。
const REBUILD_BATCH_MESSAGES: usize = 256;
/// 单次搜索最多推进的 owner 数，避免一次查询演变成全量历史重建。
const REBUILD_OWNER_LIMIT: usize = 8;
/// Store 层接受的结果硬上限；调用方的更大请求会被收紧到该值。
const MAX_SEARCH_LIMIT: usize = 100;

/// 从权威 Session/ChildTask 元数据解析出的一个独立 Conversation 索引单元。
#[derive(Clone)]
struct RecallOwner {
    owner: ConversationOwner,
    owner_kind: &'static str,
    owner_id: String,
    session_id: SessionId,
    child_task_id: Option<ChildTaskId>,
    generation: u64,
    message_count: u64,
    updated_at_ms: i64,
    path: PathBuf,
}

/// 写入一批派生文档时不随消息变化的 owner 元数据。
struct RecallDocumentContext<'a> {
    owner_kind: &'a str,
    owner_id: &'a str,
    session_id: &'a SessionId,
    child_task_id: Option<&'a ChildTaskId>,
    generation: u64,
    created_at_ms: i64,
}

impl StorageEngine {
    /// 为新建的空 Conversation 建立已同步的索引头。
    ///
    /// 新 owner 的消息数为零，因此无需读取正文即可处于 ready 状态。已存在的头不在这里
    /// 覆盖，恢复和 generation 切换由各自的显式路径负责。
    pub(super) fn initialize_recall_owner(
        &self,
        owner: &ConversationOwner,
        generation: u64,
        updated_at_ms: i64,
    ) -> StorageResult<()> {
        let (kind, owner_id, session_id, child_task_id) = owner_fields(owner);
        self.connection
            .execute(
                "INSERT INTO conversation_recall_heads (
                    owner_kind, owner_id, session_id, child_task_id, body_generation,
                    indexed_message_count, state, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 'ready', ?6)
                 ON CONFLICT(owner_kind, owner_id) DO NOTHING",
                params![
                    kind,
                    owner_id,
                    session_id.as_str(),
                    child_task_id.map(ChildTaskId::as_str),
                    to_i64(generation, "recall generation exceeds SQLite range")?,
                    updated_at_ms,
                ],
            )
            .map_err(|source| {
                internal_error("conversation recall head could not be created", source)
            })?;
        Ok(())
    }

    pub(super) fn mark_recall_owner_dirty(
        &self,
        owner: &ConversationOwner,
        generation: u64,
        updated_at_ms: i64,
    ) {
        let (kind, owner_id, session_id, child_task_id) = owner_fields(owner);
        // 该方法通常运行在权威 Conversation 已提交之后。索引降级不能覆盖原本成功的业务
        // 操作，因此这里采用 best-effort；后续搜索仍会根据权威 generation/message_count
        // 发现不一致并尝试重建。
        let _ = self.connection.execute(
            "INSERT INTO conversation_recall_heads (
                owner_kind, owner_id, session_id, child_task_id, body_generation,
                indexed_message_count, state, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 'dirty', ?6)
             ON CONFLICT(owner_kind, owner_id) DO UPDATE SET
                session_id = excluded.session_id,
                child_task_id = excluded.child_task_id,
                body_generation = excluded.body_generation,
                indexed_message_count = 0,
                state = 'dirty',
                updated_at_ms = excluded.updated_at_ms",
            params![
                kind,
                owner_id,
                session_id.as_str(),
                child_task_id.map(ChildTaskId::as_str),
                i64::try_from(generation).unwrap_or(i64::MAX),
                updated_at_ms,
            ],
        );
    }

    pub(super) fn mark_recall_owner_dirty_now(&self, owner: &ConversationOwner, generation: u64) {
        self.mark_recall_owner_dirty(owner, generation, now_ms());
    }

    pub(super) fn index_committed_recall_batch(
        &mut self,
        owner: &ConversationOwner,
        generation: u64,
        base_message_ordinal: u64,
        created_at_ms: i64,
        messages: &[ConversationMessage],
    ) {
        if !self.recall_index_available {
            return;
        }
        // 调用方只会把已经可靠提交的消息交给这里。任何索引错误都降级为 dirty，不能把
        // 派生投影失败传播成 Conversation 写入失败。
        if self
            .try_index_committed_recall_batch(
                owner,
                generation,
                base_message_ordinal,
                created_at_ms,
                messages,
            )
            .is_err()
        {
            self.mark_recall_owner_dirty(owner, generation, created_at_ms);
        }
    }

    fn try_index_committed_recall_batch(
        &mut self,
        owner: &ConversationOwner,
        generation: u64,
        base_message_ordinal: u64,
        created_at_ms: i64,
        messages: &[ConversationMessage],
    ) -> StorageResult<()> {
        let (kind, owner_id, session_id, child_task_id) = owner_fields(owner);
        let head = self
            .connection
            .query_row(
                "SELECT body_generation, indexed_message_count, state
                 FROM conversation_recall_heads WHERE owner_kind = ?1 AND owner_id = ?2",
                params![kind, owner_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("conversation recall head could not be read", source)
            })?;
        let can_append = match head {
            Some((head_generation, indexed, state)) => {
                non_negative_u64(head_generation, "recall generation is invalid")? == generation
                    && non_negative_u64(indexed, "recall message count is invalid")?
                        == base_message_ordinal
                    && state == "ready"
            }
            None => base_message_ordinal == 0,
        };
        // 增量追加只有在 generation、已索引消息数和 ready 状态全部连续时才安全。
        // 任一条件不符都可能留下缺口或混入旧 generation，因此放弃追加并等待完整重建。
        if !can_append {
            self.mark_recall_owner_dirty(owner, generation, created_at_ms);
            return Ok(());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation recall append could not begin", source)
            })?;
        let document_context = RecallDocumentContext {
            owner_kind: kind,
            owner_id: &owner_id,
            session_id,
            child_task_id,
            generation,
            created_at_ms,
        };
        // 文档和 head 进度必须在同一事务中提交。否则崩溃后无法区分“文档已写但进度未进”
        // 与“进度已进但文档未写”，也就不能安全地从 indexed_message_count 继续。
        for (offset, message) in messages.iter().enumerate() {
            let ordinal = base_message_ordinal
                .checked_add(u64::try_from(offset).map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::Internal,
                        "recall ordinal is invalid",
                        source,
                    )
                })?)
                .ok_or_else(|| {
                    StoreError::new(StoreErrorKind::Internal, "recall ordinal is invalid")
                })?;
            insert_document(&transaction, &document_context, ordinal, message)?;
        }
        let next_count = base_message_ordinal
            .checked_add(u64::try_from(messages.len()).map_err(|source| {
                StoreError::with_source(
                    StoreErrorKind::Internal,
                    "recall message count is invalid",
                    source,
                )
            })?)
            .ok_or_else(|| {
                StoreError::new(StoreErrorKind::Internal, "recall message count is invalid")
            })?;
        transaction
            .execute(
                "INSERT INTO conversation_recall_heads (
                    owner_kind, owner_id, session_id, child_task_id, body_generation,
                    indexed_message_count, state, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', ?7)
                 ON CONFLICT(owner_kind, owner_id) DO UPDATE SET
                    body_generation = excluded.body_generation,
                    indexed_message_count = excluded.indexed_message_count,
                    state = 'ready',
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    kind,
                    owner_id,
                    session_id.as_str(),
                    child_task_id.map(ChildTaskId::as_str),
                    to_i64(generation, "recall generation exceeds SQLite range")?,
                    to_i64(next_count, "recall message count exceeds SQLite range")?,
                    created_at_ms,
                ],
            )
            .map_err(|source| {
                internal_error("conversation recall head could not be advanced", source)
            })?;
        transaction.commit().map_err(|source| {
            internal_error("conversation recall append could not be committed", source)
        })
    }

    pub(super) fn search_conversations(
        &mut self,
        request: ConversationSearchRequest,
    ) -> StorageResult<ConversationSearchPage> {
        if !self.recall_index_available {
            return Err(StoreError::new(
                StoreErrorKind::Unavailable,
                "conversation recall index is unavailable",
            ));
        }
        let query = normalize_text(&request.query);
        if query.chars().count() < 3 {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "conversation recall query is too short",
            ));
        }
        let owners = self.recall_owners(&request.scope)?;
        let mut partial = false;
        let mut failed_owners = Vec::new();
        let mut rebuilt = 0_usize;
        // 搜索请求顺便推进 dirty/rebuilding owner，但一次只推进有限 owner，且每个 owner
        // 只处理一个批次。未达到 ready 的 owner 不参与本次查询，并通过 partial 告知上层。
        for owner in &owners {
            if !self.recall_owner_needs_work(owner)? {
                continue;
            }
            if rebuilt == REBUILD_OWNER_LIMIT {
                partial = true;
                continue;
            }
            rebuilt += 1;
            match self.rebuild_recall_owner_batch(owner) {
                Ok(done) => partial |= !done,
                Err(_) => {
                    self.mark_recall_owner_unavailable(owner);
                    failed_owners.push(owner.owner.clone());
                    partial = true;
                }
            }
        }

        if !owners.is_empty()
            && owners
                .iter()
                .all(|owner| self.recall_owner_is_unavailable(owner).unwrap_or(true))
        {
            // 局部失败允许返回其他 ready owner；只有范围内所有 owner 都不可用时，才把
            // 整次搜索提升为 Unavailable。
            return Err(StoreError::new(
                StoreErrorKind::Unavailable,
                "conversation recall index is unavailable",
            ));
        }

        let hits = self.query_ready_recall_documents(
            &query,
            &request.scope,
            request.limit.clamp(1, MAX_SEARCH_LIMIT),
        )?;
        Ok(ConversationSearchPage {
            hits,
            partial,
            failed_owners,
        })
    }

    fn recall_owner_needs_work(&self, owner: &RecallOwner) -> StorageResult<bool> {
        let head = self
            .connection
            .query_row(
                "SELECT body_generation, indexed_message_count, state
                 FROM conversation_recall_heads WHERE owner_kind = ?1 AND owner_id = ?2",
                params![owner.owner_kind, owner.owner_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("conversation recall head could not be read", source)
            })?;
        Ok(match head {
            Some((generation, count, state)) => {
                non_negative_u64(generation, "recall generation is invalid")? != owner.generation
                    || non_negative_u64(count, "recall message count is invalid")?
                        != owner.message_count
                    || state != "ready"
            }
            None => true,
        })
    }

    fn recall_owner_is_unavailable(&self, owner: &RecallOwner) -> StorageResult<bool> {
        self.connection
            .query_row(
                "SELECT state = 'unavailable' FROM conversation_recall_heads
                 WHERE owner_kind = ?1 AND owner_id = ?2 AND body_generation = ?3",
                params![
                    owner.owner_kind,
                    owner.owner_id,
                    to_i64(owner.generation, "recall generation exceeds SQLite range")?,
                ],
                |row| row.get(0),
            )
            .optional()
            .map(Option::unwrap_or_default)
            .map_err(|source| {
                internal_error("conversation recall head could not be checked", source)
            })
    }

    fn rebuild_recall_owner_batch(&mut self, owner: &RecallOwner) -> StorageResult<bool> {
        let head = self
            .connection
            .query_row(
                "SELECT body_generation, indexed_message_count, state
                 FROM conversation_recall_heads WHERE owner_kind = ?1 AND owner_id = ?2",
                params![owner.owner_kind, owner.owner_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("conversation recall head could not be read", source)
            })?;
        let resume = head
            .as_ref()
            .filter(|(generation, _, state)| {
                u64::try_from(*generation).ok() == Some(owner.generation) && state == "rebuilding"
            })
            .map(|(_, count, _)| non_negative_u64(*count, "recall message count is invalid"))
            .transpose()?
            .unwrap_or(0);
        if resume == 0 {
            // 新 rebuild 必须先原子清掉旧投影并把 head 切到 rebuilding。查询只读取 ready，
            // 因此重建过程中不会暴露旧 generation 或仅完成一部分的新 generation。
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    internal_error("conversation recall rebuild could not begin", source)
                })?;
            transaction
                .execute(
                    "DELETE FROM conversation_recall_documents WHERE owner_kind = ?1 AND owner_id = ?2",
                    params![owner.owner_kind, owner.owner_id],
                )
                .map_err(|source| internal_error("stale recall documents could not be removed", source))?;
            transaction
                .execute(
                    "INSERT INTO conversation_recall_heads (
                        owner_kind, owner_id, session_id, child_task_id, body_generation,
                        indexed_message_count, state, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 'rebuilding', ?6)
                     ON CONFLICT(owner_kind, owner_id) DO UPDATE SET
                        session_id = excluded.session_id,
                        child_task_id = excluded.child_task_id,
                        body_generation = excluded.body_generation,
                        indexed_message_count = 0,
                        state = 'rebuilding',
                        updated_at_ms = excluded.updated_at_ms",
                    params![
                        owner.owner_kind,
                        owner.owner_id,
                        owner.session_id.as_str(),
                        owner.child_task_id.as_ref().map(ChildTaskId::as_str),
                        to_i64(owner.generation, "recall generation exceeds SQLite range")?,
                        owner.updated_at_ms,
                    ],
                )
                .map_err(|source| {
                    internal_error(
                        "conversation recall rebuild head could not be stored",
                        source,
                    )
                })?;
            transaction.commit().map_err(|source| {
                internal_error("conversation recall rebuild could not be prepared", source)
            })?;
        }

        let start = usize::try_from(resume).map_err(|source| {
            StoreError::with_source(
                StoreErrorKind::InvalidData,
                "recall message count is invalid",
                source,
            )
        })?;
        let (snapshot, end, total) = self.conversation_indexes.read_raw_window(
            &owner.path,
            start,
            REBUILD_BATCH_MESSAGES,
        )?;
        // read_raw_window 依赖可重建的 JSONL 字节偏移，只载入当前批次，不把整份会话正文
        // 常驻内存。indexed_message_count 是下一批次的恢复游标。
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                internal_error("conversation recall rebuild batch could not begin", source)
            })?;
        let document_context = RecallDocumentContext {
            owner_kind: owner.owner_kind,
            owner_id: &owner.owner_id,
            session_id: &owner.session_id,
            child_task_id: owner.child_task_id.as_ref(),
            generation: owner.generation,
            created_at_ms: owner.updated_at_ms,
        };
        for (offset, message) in snapshot.messages.iter().enumerate() {
            insert_document(
                &transaction,
                &document_context,
                u64::try_from(start + offset).map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::Internal,
                        "recall ordinal is invalid",
                        source,
                    )
                })?,
                message,
            )?;
        }
        let state = if end == total { "ready" } else { "rebuilding" };
        transaction
            .execute(
                "UPDATE conversation_recall_heads
                 SET indexed_message_count = ?1, state = ?2, updated_at_ms = ?3
                 WHERE owner_kind = ?4 AND owner_id = ?5 AND body_generation = ?6",
                params![
                    to_i64(
                        u64::try_from(end).map_err(|source| {
                            StoreError::with_source(
                                StoreErrorKind::Internal,
                                "recall message count is invalid",
                                source,
                            )
                        })?,
                        "recall message count exceeds SQLite range"
                    )?,
                    state,
                    now_ms(),
                    owner.owner_kind,
                    owner.owner_id,
                    to_i64(owner.generation, "recall generation exceeds SQLite range")?,
                ],
            )
            .map_err(|source| {
                internal_error(
                    "conversation recall rebuild head could not be advanced",
                    source,
                )
            })?;
        transaction.commit().map_err(|source| {
            internal_error(
                "conversation recall rebuild batch could not be committed",
                source,
            )
        })?;
        Ok(end == total)
    }

    fn recall_owners(&self, scope: &ConversationSearchScope) -> StorageResult<Vec<RecallOwner>> {
        // Scope 只决定纳入哪些主 Session；每个命中的 Session 还会带上其 child Conversation。
        // owner 的 generation/message_count/path 均从权威元数据推导，不信任派生索引头。
        let session_filter = match scope {
            ConversationSearchScope::Session { .. } => "WHERE sessions.session_id = ?1",
            ConversationSearchScope::Workspace { .. } => {
                "JOIN session_resources ON session_resources.session_id = sessions.session_id WHERE session_resources.workspace_id = ?1"
            }
            ConversationSearchScope::Global => "",
        };
        let query = format!(
            "SELECT sessions.session_id, sessions.body_generation, sessions.message_count,
                    sessions.updated_at_ms FROM sessions {session_filter}
             ORDER BY sessions.updated_at_ms DESC, sessions.session_id"
        );
        let mut statement = self
            .connection
            .prepare(&query)
            .map_err(|source| internal_error("recall owners could not be queried", source))?;
        let parameter = match scope {
            ConversationSearchScope::Session { session_id } => Some(session_id.as_str()),
            ConversationSearchScope::Workspace { workspace_id } => Some(workspace_id.as_str()),
            ConversationSearchScope::Global => None,
        };
        let sessions: Vec<(String, i64, i64, i64)> = if let Some(parameter) = parameter {
            statement
                .query_map([parameter], read_recall_owner_row)
                .and_then(|rows| rows.collect())
        } else {
            statement
                .query_map([], read_recall_owner_row)
                .and_then(|rows| rows.collect())
        }
        .map_err(|source| internal_error("recall owners could not be read", source))?;
        drop(statement);

        let mut owners = Vec::new();
        for (session, generation, count, updated_at_ms) in sessions {
            let session_id = SessionId::new(session).map_err(|source| {
                StoreError::with_source(
                    StoreErrorKind::InvalidData,
                    "stored session id is invalid",
                    source,
                )
            })?;
            let generation = non_negative_u64(generation, "stored recall generation is invalid")?;
            owners.push(RecallOwner {
                owner: ConversationOwner::MainSession {
                    session_id: session_id.clone(),
                },
                owner_kind: "session",
                owner_id: session_id.as_str().to_owned(),
                session_id: session_id.clone(),
                child_task_id: None,
                generation,
                message_count: non_negative_u64(count, "stored recall message count is invalid")?,
                updated_at_ms,
                path: body_path(&self.session_directory(&session_id)?, generation),
            });
            let mut children = self
                .connection
                .prepare(
                    "SELECT child_task_id, body_generation, message_count,
                            COALESCE(finished_at_ms, started_at_ms, created_at_ms)
                     FROM child_tasks WHERE session_id = ?1
                     ORDER BY COALESCE(finished_at_ms, started_at_ms, created_at_ms) DESC,
                              child_task_id",
                )
                .map_err(|source| {
                    internal_error("child recall owners could not be queried", source)
                })?;
            let child_rows = children
                .query_map([session_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|source| internal_error("child recall owners could not be read", source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| {
                    internal_error("child recall owner row could not be read", source)
                })?;
            drop(children);
            for (child, generation, count, updated_at_ms) in child_rows {
                let child_task_id = ChildTaskId::new(child).map_err(|source| {
                    StoreError::with_source(
                        StoreErrorKind::InvalidData,
                        "stored child task id is invalid",
                        source,
                    )
                })?;
                let generation =
                    non_negative_u64(generation, "stored child recall generation is invalid")?;
                owners.push(RecallOwner {
                    owner: ConversationOwner::ChildTask {
                        session_id: session_id.clone(),
                        child_task_id: child_task_id.clone(),
                    },
                    owner_kind: "child_task",
                    owner_id: child_task_id.as_str().to_owned(),
                    session_id: session_id.clone(),
                    child_task_id: Some(child_task_id.clone()),
                    generation,
                    message_count: non_negative_u64(
                        count,
                        "stored child recall message count is invalid",
                    )?,
                    updated_at_ms,
                    path: self.child_body(&session_id, &child_task_id, generation)?,
                });
            }
        }
        Ok(owners)
    }

    fn query_ready_recall_documents(
        &self,
        query: &str,
        scope: &ConversationSearchScope,
        limit: usize,
    ) -> StorageResult<Vec<ConversationSearchHit>> {
        let scope_clause = match scope {
            ConversationSearchScope::Session { .. } => "AND documents.session_id = ?2",
            ConversationSearchScope::Workspace { .. } => {
                "AND documents.session_id IN (SELECT session_id FROM session_resources WHERE workspace_id = ?2)"
            }
            ConversationSearchScope::Global => "",
        };
        let sql = format!(
            "SELECT documents.owner_kind, documents.session_id, documents.child_task_id,
                    documents.body_generation, documents.message_id, documents.message_ordinal,
                    documents.created_at_ms, documents.normalized_text
             FROM conversation_recall_fts
             JOIN conversation_recall_documents AS documents
               ON documents.document_rowid = conversation_recall_fts.rowid
             JOIN conversation_recall_heads AS heads
               ON heads.owner_kind = documents.owner_kind AND heads.owner_id = documents.owner_id
             WHERE conversation_recall_fts.normalized_text MATCH ?1
               AND heads.state = 'ready'
               AND heads.body_generation = documents.body_generation
               AND (
                    (documents.owner_kind = 'session' AND EXISTS (
                        SELECT 1 FROM sessions AS current_session
                        WHERE current_session.session_id = documents.session_id
                          AND current_session.body_generation = documents.body_generation
                    ))
                    OR
                    (documents.owner_kind = 'child_task' AND EXISTS (
                        SELECT 1 FROM child_tasks AS current_child
                        WHERE current_child.child_task_id = documents.child_task_id
                          AND current_child.session_id = documents.session_id
                          AND current_child.body_generation = documents.body_generation
                    ))
               )
               {scope_clause}
             ORDER BY bm25(conversation_recall_fts), documents.created_at_ms DESC,
                      documents.session_id, COALESCE(documents.child_task_id, ''),
                      documents.message_ordinal
             LIMIT ?3"
        );
        // 将归一化后的用户输入作为 FTS phrase 查询，并继续通过绑定参数交给 SQLite，避免
        // 用户文本成为 FTS 查询语法。多词查询因此采用保守的连续短语语义。
        let match_query = format!("\"{}\"", query.replace('"', "\"\""));
        let scope_value = match scope {
            ConversationSearchScope::Session { session_id } => session_id.as_str(),
            ConversationSearchScope::Workspace { workspace_id } => workspace_id.as_str(),
            ConversationSearchScope::Global => "",
        };
        let mut statement = self.connection.prepare(&sql).map_err(|source| {
            internal_error("conversation recall query could not be prepared", source)
        })?;
        let rows = statement
            .query_map(
                params![
                    match_query,
                    scope_value,
                    i64::try_from(limit).unwrap_or(MAX_SEARCH_LIMIT as i64)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .map_err(|source| {
                internal_error("conversation recall query could not be executed", source)
            })?;
        let records = rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
            internal_error("conversation recall result could not be read", source)
        })?;
        records
            .into_iter()
            .map(
                |(kind, session, child, generation, message, ordinal, created_at_ms, text)| {
                    let session_id = SessionId::new(session).map_err(|source| {
                        StoreError::with_source(
                            StoreErrorKind::InvalidData,
                            "stored session id is invalid",
                            source,
                        )
                    })?;
                    let owner = match (kind.as_str(), child) {
                        ("session", None) => ConversationOwner::MainSession { session_id },
                        ("child_task", Some(child)) => ConversationOwner::ChildTask {
                            session_id,
                            child_task_id: ChildTaskId::new(child).map_err(|source| {
                                StoreError::with_source(
                                    StoreErrorKind::InvalidData,
                                    "stored child task id is invalid",
                                    source,
                                )
                            })?,
                        },
                        _ => {
                            return Err(StoreError::new(
                                StoreErrorKind::InvalidData,
                                "stored recall owner is invalid",
                            ));
                        }
                    };
                    Ok(ConversationSearchHit {
                        owner,
                        generation: non_negative_u64(
                            generation,
                            "stored recall generation is invalid",
                        )?,
                        message_id: MessageId::new(message).map_err(|source| {
                            StoreError::with_source(
                                StoreErrorKind::InvalidData,
                                "stored message id is invalid",
                                source,
                            )
                        })?,
                        message_ordinal: non_negative_u64(
                            ordinal,
                            "stored recall ordinal is invalid",
                        )?,
                        created_at_ms,
                        text,
                    })
                },
            )
            .collect()
    }

    fn mark_recall_owner_unavailable(&self, owner: &RecallOwner) {
        // 与 dirty 标记相同，这也是派生状态的 best-effort 更新。failed_owners/partial 会把
        // 本次失败反馈给调用方，不能因为状态标记失败而影响其他 owner 的查询结果。
        let _ = self.connection.execute(
            "UPDATE conversation_recall_heads SET state = 'unavailable', updated_at_ms = ?1
             WHERE owner_kind = ?2 AND owner_id = ?3",
            params![now_ms(), owner.owner_kind, owner.owner_id],
        );
    }
}

fn insert_document(
    transaction: &rusqlite::Transaction<'_>,
    context: &RecallDocumentContext<'_>,
    ordinal: u64,
    message: &ConversationMessage,
) -> StorageResult<()> {
    let Some((message_id, message_kind, text)) = searchable_message(message) else {
        return Ok(());
    };
    let normalized = normalize_text(&text);
    if normalized.is_empty() {
        return Ok(());
    }
    let content_hash = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    let document_id = format!(
        "{}:{}:{}:{}",
        context.owner_kind,
        context.owner_id,
        context.generation,
        message_id.as_str()
    );
    // document_id 包含 generation，使新旧正文不会发生键碰撞。真正可见性仍由查询阶段的
    // ready head 与权威 owner generation 双重约束，而不是仅依赖文档自身字段。
    transaction
        .execute(
            "INSERT INTO conversation_recall_documents (
                document_id, owner_kind, owner_id, session_id, child_task_id, body_generation,
                message_id, message_kind, message_ordinal, created_at_ms, normalized_text,
                content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(document_id) DO UPDATE SET
                message_ordinal = excluded.message_ordinal,
                created_at_ms = excluded.created_at_ms,
                normalized_text = excluded.normalized_text,
                content_hash = excluded.content_hash",
            params![
                document_id,
                context.owner_kind,
                context.owner_id,
                context.session_id.as_str(),
                context.child_task_id.map(ChildTaskId::as_str),
                to_i64(context.generation, "recall generation exceeds SQLite range")?,
                message_id.as_str(),
                message_kind,
                to_i64(ordinal, "recall ordinal exceeds SQLite range")?,
                context.created_at_ms,
                normalized,
                content_hash,
            ],
        )
        .map_err(|source| {
            internal_error("conversation recall document could not be stored", source)
        })?;
    Ok(())
}

fn searchable_message(message: &ConversationMessage) -> Option<(&MessageId, &'static str, String)> {
    // Recall 只检索用户在正常会话中可见的语义内容。注入内容、Reasoning、工具协议状态、
    // Tool Result、System Prompt 和 Context Summary 都不能通过搜索结果重新进入上下文。
    match message {
        ConversationMessage::User(message) if message.transcript_visibility.is_visible() => {
            let mut parts = Vec::new();
            for part in &message.parts {
                match part {
                    UserPart::Text(part) => parts.push(part.text.clone()),
                    UserPart::FileReferences(references) => parts.extend(
                        references
                            .files
                            .iter()
                            .map(|file| file.original_name.clone()),
                    ),
                    UserPart::QuotedText(quoted) => parts.push(quoted.exact.clone()),
                    UserPart::Injected(_) | UserPart::InternalContext(_) => {}
                }
            }
            Some((&message.id, "user", parts.join("\n")))
        }
        ConversationMessage::Assistant(message) => {
            let text = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(part) => Some(part.text.as_str()),
                    AssistantPart::Reasoning(_)
                    | AssistantPart::ToolCall(_)
                    | AssistantPart::ProviderState(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some((&message.id, "assistant", text))
        }
        ConversationMessage::User(_)
        | ConversationMessage::System(_)
        | ConversationMessage::ContextSummary(_)
        | ConversationMessage::Tool(_) => None,
    }
}

fn owner_fields(
    owner: &ConversationOwner,
) -> (&'static str, String, &SessionId, Option<&ChildTaskId>) {
    match owner {
        ConversationOwner::MainSession { session_id } => {
            ("session", session_id.as_str().to_owned(), session_id, None)
        }
        ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        } => (
            "child_task",
            child_task_id.as_str().to_owned(),
            session_id,
            Some(child_task_id),
        ),
    }
}

fn read_recall_owner_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, i64, i64, i64)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
