//! Persona 与 Pinned Memory 的 SQLite 权威实现。

use std::collections::BTreeMap;

use agent_memory::{MemoryPropertyValue, PinnedMemoryCategory, PinnedMemoryEntry, PinnedMemoryId};
use assistant_protocol::SessionId;
use assistant_runtime::{
    MemoryContextSnapshot, PersonaMutation, PersonaSnapshot, PinnedMemoryCreatedBy,
    PinnedMemoryMutation, PinnedMemoryMutationResult, StoredPinnedMemory,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error,
    filesystem::{non_negative_u64, to_i64},
    invalid_data_with_source,
};

impl StorageEngine {
    pub(super) fn load_memory_context(&self) -> StorageResult<MemoryContextSnapshot> {
        Ok(MemoryContextSnapshot {
            persona: self.get_persona()?,
            pinned_collection_revision: self.pinned_collection_revision()?,
            pinned_memories: self.list_pinned_memories()?,
        })
    }

    pub(super) fn get_persona(&self) -> StorageResult<PersonaSnapshot> {
        self.connection
            .query_row(
                "SELECT enabled, content, revision, updated_at_ms
                 FROM persona WHERE singleton_key = 1",
                [],
                |row| {
                    Ok(PersonaSnapshot {
                        enabled: row.get::<_, i64>(0)? != 0,
                        content: row.get(1)?,
                        revision: decode_revision(row.get(2)?)?,
                        updated_at_ms: row.get(3)?,
                    })
                },
            )
            .map_err(|source| invalid_data_with_source("persona state is unavailable", source))
    }

    pub(super) fn set_persona(
        &mut self,
        mutation: PersonaMutation,
    ) -> StorageResult<PersonaSnapshot> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("persona transaction could not be started", source)
            })?;
        let next_revision = mutation
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| conflict("persona revision exhausted"))?;
        let next_revision_sql = to_i64(next_revision, "persona revision is too large")?;
        let expected_revision_sql = to_i64(
            mutation.expected_revision,
            "expected persona revision is too large",
        )?;
        let changed = transaction
            .execute(
                "UPDATE persona SET enabled = ?1, content = ?2, revision = ?3, updated_at_ms = ?4
                 WHERE singleton_key = 1 AND revision = ?5",
                params![
                    if mutation.enabled { 1_i64 } else { 0_i64 },
                    mutation.content,
                    next_revision_sql,
                    mutation.updated_at_ms,
                    expected_revision_sql,
                ],
            )
            .map_err(|source| database_write_error("persona could not be updated", source))?;
        if changed != 1 {
            return Err(conflict("persona revision changed"));
        }
        transaction.commit().map_err(|source| {
            database_write_error("persona update could not be committed", source)
        })?;
        Ok(PersonaSnapshot {
            enabled: mutation.enabled,
            content: mutation.content,
            revision: next_revision,
            updated_at_ms: mutation.updated_at_ms,
        })
    }

    pub(super) fn list_pinned_memories(&self) -> StorageResult<Vec<StoredPinnedMemory>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, category, content, attributes_json, created_by_kind,
                        created_by_session_id, revision, created_at_ms, updated_at_ms
                 FROM pinned_memories ORDER BY id",
            )
            .map_err(|source| {
                invalid_data_with_source("pinned memories are unavailable", source)
            })?;
        let rows = statement
            .query_map([], decode_pinned_memory)
            .map_err(|source| {
                invalid_data_with_source("pinned memories are unavailable", source)
            })?;
        rows.map(|row| {
            row.map_err(|source| invalid_data_with_source("pinned memory is invalid", source))
        })
        .collect()
    }

    pub(super) fn mutate_pinned_memory(
        &mut self,
        mutation: PinnedMemoryMutation,
    ) -> StorageResult<PinnedMemoryMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("pinned memory transaction could not be started", source)
            })?;
        let memory = match mutation {
            PinnedMemoryMutation::Create {
                entry,
                created_by,
                expected_collection_revision,
                changed_at_ms,
            } => {
                require_collection_revision(&transaction, expected_collection_revision)?;
                let attributes = serde_json::to_string(&entry.attributes).map_err(|source| {
                    invalid_data_with_source("pinned memory attributes are invalid", source)
                })?;
                let (created_by_kind, created_by_session_id) = created_by_fields(&created_by);
                transaction
                    .execute(
                        "INSERT INTO pinned_memories (
                            id, category, content, attributes_json, created_by_kind,
                            created_by_session_id, revision, created_at_ms, updated_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
                        params![
                            entry.id.as_str(),
                            entry.category.as_str(),
                            entry.content,
                            attributes,
                            created_by_kind,
                            created_by_session_id,
                            changed_at_ms,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("pinned memory could not be created", source)
                    })?;
                Some(StoredPinnedMemory {
                    entry,
                    created_by,
                    created_at_ms: changed_at_ms,
                    updated_at_ms: changed_at_ms,
                    revision: 1,
                })
            }
            PinnedMemoryMutation::Replace {
                entry,
                expected_revision,
                changed_at_ms,
            } => {
                let existing = load_pinned_memory(&transaction, entry.id.as_str())?
                    .ok_or_else(|| conflict("pinned memory does not exist"))?;
                let next_revision = expected_revision
                    .checked_add(1)
                    .ok_or_else(|| conflict("pinned memory revision exhausted"))?;
                let next_revision_sql =
                    to_i64(next_revision, "pinned memory revision is too large")?;
                let expected_revision_sql = to_i64(
                    expected_revision,
                    "expected pinned memory revision is too large",
                )?;
                let attributes = serde_json::to_string(&entry.attributes).map_err(|source| {
                    invalid_data_with_source("pinned memory attributes are invalid", source)
                })?;
                let changed = transaction
                    .execute(
                        "UPDATE pinned_memories
                         SET category = ?1, content = ?2, attributes_json = ?3,
                             revision = ?4, updated_at_ms = ?5
                         WHERE id = ?6 AND revision = ?7",
                        params![
                            entry.category.as_str(),
                            entry.content,
                            attributes,
                            next_revision_sql,
                            changed_at_ms,
                            entry.id.as_str(),
                            expected_revision_sql,
                        ],
                    )
                    .map_err(|source| {
                        database_write_error("pinned memory could not be updated", source)
                    })?;
                if changed != 1 {
                    return Err(conflict("pinned memory revision changed"));
                }
                Some(StoredPinnedMemory {
                    entry,
                    created_by: existing.created_by,
                    created_at_ms: existing.created_at_ms,
                    updated_at_ms: changed_at_ms,
                    revision: next_revision,
                })
            }
            PinnedMemoryMutation::Delete {
                id,
                expected_revision,
                changed_at_ms: _,
            } => {
                let expected_revision_sql = to_i64(
                    expected_revision,
                    "expected pinned memory revision is too large",
                )?;
                let changed = transaction
                    .execute(
                        "DELETE FROM pinned_memories WHERE id = ?1 AND revision = ?2",
                        params![id.as_str(), expected_revision_sql],
                    )
                    .map_err(|source| {
                        database_write_error("pinned memory could not be deleted", source)
                    })?;
                if changed != 1 {
                    return Err(conflict("pinned memory revision changed"));
                }
                None
            }
        };
        let collection_revision = increment_collection_revision(&transaction)?;
        transaction.commit().map_err(|source| {
            database_write_error("pinned memory update could not be committed", source)
        })?;
        Ok(PinnedMemoryMutationResult {
            memory,
            collection_revision,
        })
    }

    fn pinned_collection_revision(&self) -> StorageResult<u64> {
        let revision = self
            .connection
            .query_row(
                "SELECT pinned_collection_revision FROM memory_state WHERE singleton_key = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| {
                invalid_data_with_source("pinned memory state is unavailable", source)
            })?;
        non_negative_u64(revision, "pinned memory collection revision is invalid")
    }
}

fn created_by_fields(created_by: &PinnedMemoryCreatedBy) -> (&'static str, Option<&str>) {
    match created_by {
        PinnedMemoryCreatedBy::User => ("user", None),
        PinnedMemoryCreatedBy::AgentTool { session_id } => {
            ("agent_tool", Some(session_id.as_str()))
        }
    }
}

fn require_collection_revision(
    transaction: &Transaction<'_>,
    expected_revision: u64,
) -> StorageResult<()> {
    let current = transaction
        .query_row(
            "SELECT pinned_collection_revision FROM memory_state WHERE singleton_key = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| invalid_data_with_source("pinned memory state is unavailable", source))?;
    let current = non_negative_u64(current, "pinned memory collection revision is invalid")?;
    if current != expected_revision {
        return Err(conflict("pinned memory collection revision changed"));
    }
    Ok(())
}

fn increment_collection_revision(transaction: &Transaction<'_>) -> StorageResult<u64> {
    transaction
        .execute(
            "UPDATE memory_state
             SET pinned_collection_revision = pinned_collection_revision + 1
             WHERE singleton_key = 1",
            [],
        )
        .map_err(|source| {
            database_write_error("pinned memory revision could not be updated", source)
        })?;
    let revision = transaction
        .query_row(
            "SELECT pinned_collection_revision FROM memory_state WHERE singleton_key = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|source| invalid_data_with_source("pinned memory state is unavailable", source))?;
    non_negative_u64(revision, "pinned memory collection revision is invalid")
}

fn load_pinned_memory(
    transaction: &Transaction<'_>,
    id: &str,
) -> StorageResult<Option<StoredPinnedMemory>> {
    transaction
        .query_row(
            "SELECT id, category, content, attributes_json, created_by_kind,
                    created_by_session_id, revision, created_at_ms, updated_at_ms
             FROM pinned_memories WHERE id = ?1",
            [id],
            decode_pinned_memory,
        )
        .optional()
        .map_err(|source| invalid_data_with_source("pinned memory is invalid", source))
}

fn decode_pinned_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPinnedMemory> {
    let id = row.get::<_, String>(0)?;
    let category = row.get::<_, String>(1)?;
    let attributes_json = row.get::<_, String>(3)?;
    let created_by_kind = row.get::<_, String>(4)?;
    let created_by_session_id = row.get::<_, Option<String>>(5)?;
    let entry = PinnedMemoryEntry {
        id: PinnedMemoryId::new(id).map_err(to_sql_decode_error)?,
        category: PinnedMemoryCategory::new(category).map_err(to_sql_decode_error)?,
        content: row.get(2)?,
        attributes: serde_json::from_str::<BTreeMap<String, MemoryPropertyValue>>(&attributes_json)
            .map_err(to_sql_decode_error)?,
    };
    let created_by = match (created_by_kind.as_str(), created_by_session_id) {
        ("user", None) => PinnedMemoryCreatedBy::User,
        ("agent_tool", Some(session_id)) => PinnedMemoryCreatedBy::AgentTool {
            session_id: SessionId::new(session_id).map_err(to_sql_decode_error)?,
        },
        _ => {
            return Err(to_sql_decode_error(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid pinned memory creator",
            )));
        }
    };
    Ok(StoredPinnedMemory {
        entry,
        created_by,
        revision: decode_revision(row.get(6)?)?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

fn to_sql_decode_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn decode_revision(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(to_sql_decode_error)
}
