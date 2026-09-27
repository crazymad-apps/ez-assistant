//! 个人自定义智能体的 SQLite 内容 CAS；不缓存企业 Agent 配置。

use assistant_protocol::CustomAgentConfig;
use assistant_runtime::{
    CustomAgentMutation, StoredCustomAgent, custom_agent_revision, valid_custom_agent_config,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, invalid_data,
    invalid_data_with_source,
};

impl StorageEngine {
    pub(super) fn list_custom_agents(&self) -> StorageResult<Vec<StoredCustomAgent>> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM custom_agents ORDER BY name")
            .map_err(|source| {
                invalid_data_with_source("custom agents could not be listed", source)
            })?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| {
                invalid_data_with_source("custom agents could not be listed", source)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| invalid_data_with_source("custom agent name is invalid", source))?;
        names
            .iter()
            .map(|name| {
                load_agent(&self.connection, name)?
                    .ok_or_else(|| invalid_data("custom agent disappeared"))
            })
            .collect()
    }

    pub(super) fn mutate_custom_agent(
        &mut self,
        mutation: CustomAgentMutation,
    ) -> StorageResult<Option<StoredCustomAgent>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("custom agent transaction could not start", source)
            })?;
        let outcome = match mutation {
            CustomAgentMutation::Save {
                expected_revision,
                next,
            } => {
                if !valid_custom_agent_config(&next.config)
                    || next.created_at_ms > next.updated_at_ms
                {
                    return Err(invalid_data("custom agent config is invalid"));
                }
                let current = load_agent(&transaction, &next.config.name)?;
                if current
                    .as_ref()
                    .map(|agent| custom_agent_revision(&agent.config))
                    != expected_revision
                    || current
                        .as_ref()
                        .is_some_and(|agent| agent.created_at_ms != next.created_at_ms)
                {
                    return Err(conflict("custom agent config changed"));
                }
                let document = serde_json::to_string(&next.config)
                    .map_err(|_| invalid_data("custom agent config is invalid"))?;
                let created = timestamp(next.created_at_ms)?;
                let updated = timestamp(next.updated_at_ms)?;
                if current.is_some() {
                    let changed = transaction
                        .execute(
                            "UPDATE custom_agents SET config_json=?1, updated_at_ms=?2
                         WHERE name=?3 AND created_at_ms=?4",
                            params![document, updated, next.config.name, created],
                        )
                        .map_err(|source| {
                            database_write_error("custom agent could not be updated", source)
                        })?;
                    if changed != 1 {
                        return Err(conflict("custom agent config changed"));
                    }
                } else {
                    transaction.execute(
                        "INSERT INTO custom_agents(name, config_json, created_at_ms, updated_at_ms)
                         VALUES(?1, ?2, ?3, ?4)",
                        params![next.config.name, document, created, updated],
                    ).map_err(|source| database_write_error("custom agent could not be created", source))?;
                }
                Some(next)
            }
            CustomAgentMutation::Delete {
                name,
                expected_revision,
            } => {
                let current = load_agent(&transaction, &name)?
                    .ok_or_else(|| conflict("custom agent is unavailable"))?;
                if custom_agent_revision(&current.config) != expected_revision {
                    return Err(conflict("custom agent config changed"));
                }
                let changed = transaction
                    .execute(
                        "DELETE FROM custom_agents WHERE name=?1 AND created_at_ms=?2",
                        params![name, timestamp(current.created_at_ms)?],
                    )
                    .map_err(|source| {
                        database_write_error("custom agent could not be deleted", source)
                    })?;
                if changed != 1 {
                    return Err(conflict("custom agent config changed"));
                }
                None
            }
        };
        transaction.commit().map_err(|source| {
            database_write_error("custom agent change could not be committed", source)
        })?;
        Ok(outcome)
    }
}

fn load_agent(connection: &Connection, name: &str) -> StorageResult<Option<StoredCustomAgent>> {
    let row = connection
        .query_row(
            "SELECT config_json, created_at_ms, updated_at_ms FROM custom_agents WHERE name=?1",
            [name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|source| invalid_data_with_source("custom agent could not be loaded", source))?;
    row.map(|(document, created, updated)| {
        let config = serde_json::from_str::<CustomAgentConfig>(&document)
            .map_err(|source| invalid_data_with_source("custom agent config is invalid", source))?;
        if config.name != name || !valid_custom_agent_config(&config) {
            return Err(invalid_data("custom agent config is invalid"));
        }
        Ok(StoredCustomAgent {
            config,
            created_at_ms: u64::try_from(created)
                .map_err(|_| invalid_data("custom agent timestamp is invalid"))?,
            updated_at_ms: u64::try_from(updated)
                .map_err(|_| invalid_data("custom agent timestamp is invalid"))?,
        })
    })
    .transpose()
}

fn timestamp(value: u64) -> StorageResult<i64> {
    i64::try_from(value).map_err(|_| invalid_data("custom agent timestamp is invalid"))
}
