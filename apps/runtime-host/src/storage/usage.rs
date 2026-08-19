//! 已提交模型请求事实与 Session 用量滚动汇总。

use agent_types::{ConversationMessage, ModelIdentity, TokenUsage};
use assistant_protocol::{ConversationOwner, SessionId};
use assistant_runtime::StoredSessionUsage;
use rusqlite::{OptionalExtension, Transaction, params};

use super::{StorageEngine, StorageResult, internal_error, non_negative_u64, to_i64};

struct UsageRecord<'a> {
    request_id: String,
    request_kind: &'static str,
    model: Option<&'a ModelIdentity>,
    usage: &'a TokenUsage,
}

impl StorageEngine {
    pub(super) fn get_session_usage(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<StoredSessionUsage> {
        self.connection
            .query_row(
                "SELECT request_count, input_tokens_sum, output_tokens_sum, total_tokens_sum,
                        cached_input_tokens_sum, cached_request_count, reasoning_tokens_sum,
                        reasoning_request_count, latest_input_tokens, latest_output_tokens,
                        latest_total_tokens, latest_cached_input_tokens, latest_reasoning_tokens
                 FROM session_usage WHERE session_id = ?1",
                [session_id.as_str()],
                decode_session_usage,
            )
            .optional()
            .map_err(|source| internal_error("session usage could not be read", source))
            .map(|usage| usage.unwrap_or_default())
    }

    pub(super) fn backfill_session_usage(&mut self) -> StorageResult<()> {
        let pending = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT session_id, updated_at_ms FROM session_usage
                     WHERE backfilled = 0 ORDER BY session_id",
                )
                .map_err(|source| {
                    internal_error(
                        "pending session usage backfill could not be queried",
                        source,
                    )
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|source| {
                    internal_error("pending session usage backfill could not be read", source)
                })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                internal_error(
                    "pending session usage backfill row could not be read",
                    source,
                )
            })?
        };

        for (session_id, completed_at_ms) in pending {
            if self.unavailable_sessions.contains(&session_id) {
                continue;
            }
            let session_id = SessionId::new(session_id).map_err(|source| {
                assistant_runtime::StoreError::with_source(
                    assistant_runtime::StoreErrorKind::InvalidData,
                    "session usage backfill identity is invalid",
                    source,
                )
            })?;
            let snapshot = match self.load_conversation(&session_id) {
                Ok(snapshot) => snapshot,
                Err(_) => continue,
            };
            let owner = ConversationOwner::MainSession {
                session_id: session_id.clone(),
            };
            let transaction = self.connection.transaction().map_err(|source| {
                internal_error("session usage backfill could not begin", source)
            })?;
            record_usage_messages(
                &transaction,
                &owner,
                None,
                &snapshot.messages,
                completed_at_ms,
                true,
            )?;
            transaction
                .execute(
                    "UPDATE session_usage SET backfilled = 1 WHERE session_id = ?1",
                    [session_id.as_str()],
                )
                .map_err(|source| {
                    internal_error("session usage backfill could not be marked", source)
                })?;
            transaction.commit().map_err(|source| {
                internal_error("session usage backfill could not be committed", source)
            })?;
        }
        Ok(())
    }
}

pub(super) fn record_usage_messages(
    transaction: &Transaction<'_>,
    owner: &ConversationOwner,
    run_id: Option<&str>,
    messages: &[ConversationMessage],
    completed_at_ms: i64,
    include_legacy_compacted: bool,
) -> StorageResult<()> {
    for message in messages {
        match message {
            ConversationMessage::Assistant(message) => {
                if let Some(usage) = message.usage.as_ref() {
                    record_usage(
                        transaction,
                        owner,
                        run_id,
                        UsageRecord {
                            request_id: message.id.as_str().to_owned(),
                            request_kind: "agent_turn",
                            model: Some(&message.model),
                            usage,
                        },
                        completed_at_ms,
                    )?;
                }
            }
            ConversationMessage::ContextSummary(message) => {
                if include_legacy_compacted && let Some(usage) = message.compacted_usage.as_ref() {
                    record_usage(
                        transaction,
                        owner,
                        run_id,
                        UsageRecord {
                            request_id: format!("{}:legacy-compacted", message.id.as_str()),
                            request_kind: "legacy_compacted",
                            model: message.model.as_ref(),
                            usage,
                        },
                        completed_at_ms,
                    )?;
                }
                if let Some(usage) = message.usage.as_ref() {
                    record_usage(
                        transaction,
                        owner,
                        run_id,
                        UsageRecord {
                            request_id: message.id.as_str().to_owned(),
                            request_kind: "context_summary",
                            model: message.model.as_ref(),
                            usage,
                        },
                        completed_at_ms,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn record_usage(
    transaction: &Transaction<'_>,
    owner: &ConversationOwner,
    run_id: Option<&str>,
    record: UsageRecord<'_>,
    completed_at_ms: i64,
) -> StorageResult<()> {
    let (session_id, owner_kind, owner_id) = match owner {
        ConversationOwner::MainSession { session_id } => {
            (session_id.as_str(), "session", session_id.as_str())
        }
        ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        } => (session_id.as_str(), "child_task", child_task_id.as_str()),
    };
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO model_request_records (
                session_id, owner_kind, owner_id, request_id, run_id, request_kind,
                provider, model_id, input_tokens, output_tokens, total_tokens,
                cached_input_tokens, reasoning_tokens, completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                session_id,
                owner_kind,
                owner_id,
                record.request_id,
                run_id,
                record.request_kind,
                record.model.map(|model| model.provider.as_str()),
                record.model.map(|model| model.model.as_str()),
                to_i64(
                    record.usage.input_tokens,
                    "input token usage exceeds SQLite range"
                )?,
                to_i64(
                    record.usage.output_tokens,
                    "output token usage exceeds SQLite range"
                )?,
                to_i64(
                    record.usage.total_tokens,
                    "total token usage exceeds SQLite range"
                )?,
                record
                    .usage
                    .cached_input_tokens
                    .map(|value| to_i64(value, "cached token usage exceeds SQLite range"))
                    .transpose()?,
                record
                    .usage
                    .reasoning_tokens
                    .map(|value| to_i64(value, "reasoning token usage exceeds SQLite range"))
                    .transpose()?,
                completed_at_ms,
            ],
        )
        .map_err(|source| internal_error("model request usage could not be recorded", source))?;
    if inserted == 0 || owner_kind != "session" {
        return Ok(());
    }

    let updated = transaction
        .execute(
            "UPDATE session_usage SET
                request_count = request_count + 1,
                input_tokens_sum = input_tokens_sum + ?1,
                output_tokens_sum = output_tokens_sum + ?2,
                total_tokens_sum = total_tokens_sum + ?3,
                cached_input_tokens_sum = cached_input_tokens_sum + COALESCE(?4, 0),
                cached_request_count = cached_request_count + CASE WHEN ?4 IS NULL THEN 0 ELSE 1 END,
                reasoning_tokens_sum = reasoning_tokens_sum + COALESCE(?5, 0),
                reasoning_request_count = reasoning_request_count + CASE WHEN ?5 IS NULL THEN 0 ELSE 1 END,
                latest_input_tokens = ?1,
                latest_output_tokens = ?2,
                latest_total_tokens = ?3,
                latest_cached_input_tokens = ?4,
                latest_reasoning_tokens = ?5,
                updated_at_ms = ?6
             WHERE session_id = ?7",
            params![
                to_i64(record.usage.input_tokens, "input token usage exceeds SQLite range")?,
                to_i64(record.usage.output_tokens, "output token usage exceeds SQLite range")?,
                to_i64(record.usage.total_tokens, "total token usage exceeds SQLite range")?,
                record
                    .usage
                    .cached_input_tokens
                    .map(|value| to_i64(value, "cached token usage exceeds SQLite range"))
                    .transpose()?,
                record
                    .usage
                    .reasoning_tokens
                    .map(|value| to_i64(value, "reasoning token usage exceeds SQLite range"))
                    .transpose()?,
                completed_at_ms,
                session_id,
            ],
        )
        .map_err(|source| internal_error("session usage could not be accumulated", source))?;
    if updated != 1 {
        return Err(assistant_runtime::StoreError::new(
            assistant_runtime::StoreErrorKind::InvalidData,
            "session usage row is missing",
        ));
    }
    Ok(())
}

fn decode_session_usage(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSessionUsage> {
    let request_count = non_negative_u64(row.get(0)?, "session request count is invalid")
        .map_err(to_sqlite_error)?;
    let latest_input = row.get::<_, Option<i64>>(8)?;
    Ok(StoredSessionUsage {
        request_count,
        input_tokens: decode_u64(row, 1, "session input token sum is invalid")?,
        output_tokens: decode_u64(row, 2, "session output token sum is invalid")?,
        total_tokens: decode_u64(row, 3, "session total token sum is invalid")?,
        cached_input_tokens: decode_u64(row, 4, "session cached token sum is invalid")?,
        cached_request_count: decode_u64(row, 5, "session cached request count is invalid")?,
        reasoning_tokens: decode_u64(row, 6, "session reasoning token sum is invalid")?,
        reasoning_request_count: decode_u64(row, 7, "session reasoning request count is invalid")?,
        latest: latest_input
            .map(|input_tokens| -> rusqlite::Result<TokenUsage> {
                Ok(TokenUsage {
                    input_tokens: non_negative_u64(
                        input_tokens,
                        "latest input token usage is invalid",
                    )
                    .map_err(to_sqlite_error)?,
                    output_tokens: decode_u64(row, 9, "latest output token usage is invalid")?,
                    total_tokens: decode_u64(row, 10, "latest total token usage is invalid")?,
                    cached_input_tokens: decode_optional_u64(
                        row,
                        11,
                        "latest cached token usage is invalid",
                    )?,
                    reasoning_tokens: decode_optional_u64(
                        row,
                        12,
                        "latest reasoning token usage is invalid",
                    )?,
                })
            })
            .transpose()?,
    })
}

fn decode_u64(
    row: &rusqlite::Row<'_>,
    index: usize,
    message: &'static str,
) -> rusqlite::Result<u64> {
    non_negative_u64(row.get(index)?, message).map_err(to_sqlite_error)
}

fn decode_optional_u64(
    row: &rusqlite::Row<'_>,
    index: usize,
    message: &'static str,
) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| non_negative_u64(value, message))
        .transpose()
        .map_err(to_sqlite_error)
}

fn to_sqlite_error(error: assistant_runtime::StoreError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(error))
}
