//! Skill 名称启停与 Activation ledger 的 SQLite 物理实现。

use agent_types::MessageId;
use assistant_protocol::{InputId, RunId, SessionId};
use assistant_runtime::{
    SkillActivationOwner, SkillActivationTrigger, SkillName, SkillNameState, SkillNameStateChange,
    StoredSkillActivation,
};
use rusqlite::{Transaction, TransactionBehavior, params};

use super::{StorageEngine, StorageResult, database_write_error, invalid_data_with_source};

impl StorageEngine {
    /// 按名称稳定读取全部显式开关；缺少行仍由 Runtime 解释为默认启用。
    pub(super) fn list_skill_name_states(&self) -> StorageResult<Vec<SkillNameState>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT name, enabled, updated_at_ms
                 FROM skill_name_states
                 ORDER BY name",
            )
            .map_err(|source| {
                invalid_data_with_source("skill name states could not be loaded", source)
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|source| {
                invalid_data_with_source("skill name states could not be loaded", source)
            })?;
        rows.map(|row| {
            let (name, enabled, updated_at_ms) = row.map_err(|source| {
                invalid_data_with_source("skill name state is invalid", source)
            })?;
            // SQLite 是持久化边界，旧数据或手工写入也必须重新经过领域校验。
            let name = SkillName::parse(name).map_err(|source| {
                invalid_data_with_source("skill name state is invalid", source)
            })?;
            let enabled = match enabled {
                0 => false,
                1 => true,
                _ => return Err(super::invalid_data("skill name state is invalid")),
            };
            let updated_at_ms = u64::try_from(updated_at_ms).map_err(|source| {
                invalid_data_with_source("skill name state is invalid", source)
            })?;
            Ok(SkillNameState {
                name,
                enabled,
                updated_at_ms,
            })
        })
        .collect()
    }

    /// 在单个 SQLite 事务中按校验后的逻辑名称插入或替换开关。
    pub(super) fn set_skill_enabled(
        &mut self,
        change: SkillNameStateChange,
    ) -> StorageResult<SkillNameState> {
        let updated_at_ms = i64::try_from(change.updated_at_ms).map_err(|source| {
            invalid_data_with_source("skill name state timestamp is invalid", source)
        })?;
        // 单名称 upsert 仍使用 Immediate transaction，成功返回前必须完成提交。
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("skill name state transaction could not be started", source)
            })?;
        transaction
            .execute(
                "INSERT INTO skill_name_states (name, enabled, updated_at_ms)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET
                    enabled = excluded.enabled,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    change.name.as_str(),
                    i64::from(change.enabled),
                    updated_at_ms
                ],
            )
            .map_err(|source| {
                database_write_error("skill name state could not be saved", source)
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("skill name state could not be committed", source)
        })?;
        Ok(SkillNameState {
            name: change.name,
            enabled: change.enabled,
            updated_at_ms: change.updated_at_ms,
        })
    }

    /// 按规范 Conversation 顺序恢复全部 Session Activation 事实。
    pub(super) fn load_skill_activations(&self) -> StorageResult<Vec<StoredSkillActivation>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT activation_id, session_id, owner_kind, owner_id, run_id, input_id,
                        message_id, name, catalog_revision, definition_digest, trigger,
                        created_at_ms
                 FROM skill_activations
                 ORDER BY created_at_ms, activation_id",
            )
            .map_err(|source| {
                invalid_data_with_source("skill activations could not be loaded", source)
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            })
            .map_err(|source| {
                invalid_data_with_source("skill activations could not be loaded", source)
            })?;
        rows.map(|row| {
            let (
                activation_id,
                session_id,
                owner_kind,
                owner_id,
                run_id,
                input_id,
                message_id,
                name,
                catalog_revision,
                definition_digest,
                trigger,
                created_at_ms,
            ) = row.map_err(|source| {
                invalid_data_with_source("skill activation row is invalid", source)
            })?;
            let session_id = SessionId::new(session_id).map_err(|source| {
                invalid_data_with_source("skill activation session id is invalid", source)
            })?;
            let owner = match owner_kind.as_str() {
                "session" if owner_id == session_id.as_str() => {
                    SkillActivationOwner::Session(session_id.clone())
                }
                "child_task" if !owner_id.is_empty() => SkillActivationOwner::ChildTask(owner_id),
                _ => return Err(super::invalid_data("skill activation owner is invalid")),
            };
            let activation = StoredSkillActivation {
                activation_id,
                session_id,
                owner,
                run_id: run_id.map(RunId::new).transpose().map_err(|source| {
                    invalid_data_with_source("skill activation run id is invalid", source)
                })?,
                input_id: input_id.map(InputId::new).transpose().map_err(|source| {
                    invalid_data_with_source("skill activation input id is invalid", source)
                })?,
                message_id: MessageId::new(message_id).map_err(|source| {
                    invalid_data_with_source("skill activation message id is invalid", source)
                })?,
                name: SkillName::parse(name).map_err(|source| {
                    invalid_data_with_source("skill activation name is invalid", source)
                })?,
                catalog_revision,
                definition_digest,
                trigger: match trigger.as_str() {
                    "user" => SkillActivationTrigger::User,
                    "model" => SkillActivationTrigger::Model,
                    _ => return Err(super::invalid_data("skill activation trigger is invalid")),
                },
                created_at_ms,
            };
            validate_activation(&activation)?;
            Ok(activation)
        })
        .collect()
    }
}

pub(super) fn insert_skill_activation(
    transaction: &Transaction<'_>,
    activation: &StoredSkillActivation,
) -> StorageResult<()> {
    validate_activation(activation)?;
    let (owner_kind, owner_id) = match &activation.owner {
        SkillActivationOwner::Session(session_id) => ("session", session_id.as_str()),
        SkillActivationOwner::ChildTask(child_task_id) => ("child_task", child_task_id.as_str()),
    };
    transaction
        .execute(
            "INSERT INTO skill_activations (
                activation_id, session_id, owner_kind, owner_id, run_id, input_id,
                message_id, name, catalog_revision, definition_digest, trigger, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                activation.activation_id,
                activation.session_id.as_str(),
                owner_kind,
                owner_id,
                activation.run_id.as_ref().map(RunId::as_str),
                activation.input_id.as_ref().map(InputId::as_str),
                activation.message_id.as_str(),
                activation.name.as_str(),
                activation.catalog_revision,
                activation.definition_digest,
                match activation.trigger {
                    SkillActivationTrigger::User => "user",
                    SkillActivationTrigger::Model => "model",
                },
                activation.created_at_ms,
            ],
        )
        .map_err(|source| database_write_error("skill activation could not be saved", source))?;
    Ok(())
}

fn validate_activation(activation: &StoredSkillActivation) -> StorageResult<()> {
    let owner_matches = match &activation.owner {
        SkillActivationOwner::Session(session_id) => session_id == &activation.session_id,
        SkillActivationOwner::ChildTask(child_task_id) => !child_task_id.is_empty(),
    };
    if activation.activation_id.is_empty()
        || activation.activation_id.len() > 128
        || !owner_matches
        || activation.catalog_revision.is_empty()
        || activation.definition_digest.is_empty()
    {
        return Err(super::invalid_data("skill activation is invalid"));
    }
    Ok(())
}
