//! 服务商、全局选择与用户固定参数的 SQLite 业务操作。在线目录从不进入此模块。
use super::{StorageEngine, StorageResult, internal_error, invalid_data};
use assistant_protocol::{
    ModelFixedConfig, ModelParameters, ModelSelection, ModelSettings, ModelTokenLimit,
    ProviderConnection, ProviderInstanceId, ProviderSessionUsage, ProviderUsage, SecretValue,
    SessionId,
};
use assistant_runtime::StoredProvider;
use rusqlite::{OptionalExtension, Row, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::num::NonZeroU64;

#[cfg(test)]
mod tests;

const FIXED_COLUMNS: &str = "provider_instance_id, model_id, context_window_tokens, max_output_tokens, max_input_tokens, mode_limits_json, capabilities_json, updated_at_ms, origin";

impl StorageEngine {
    pub(super) fn load_providers(&self) -> StorageResult<Vec<StoredProvider>> {
        let mut statement = sql(self.connection.prepare("SELECT provider_instance_id, display_name, provider_type, endpoint, api_key, protocol_preference, models_path, discovery_format FROM providers ORDER BY provider_instance_id"))?;
        let mut rows = sql(statement.query([]))?;
        let mut result = Vec::new();
        while let Some(row) = sql(rows.next())? {
            result.push(StoredProvider {
                provider_instance_id: identifier(sql(row.get(0))?)?,
                connection: ProviderConnection {
                    display_name: sql(row.get(1))?,
                    provider_type: enum_read(sql(row.get(2))?)?,
                    endpoint: sql(row.get(3))?,
                    protocol_preference: enum_read(sql(row.get(5))?)?,
                    models_path: sql(row.get(6))?,
                    discovery_format: enum_read(sql(row.get(7))?)?,
                },
                api_key: SecretValue::new(sql(row.get(4))?),
            });
        }
        Ok(result)
    }
    pub(super) fn put_provider(&self, provider: StoredProvider) -> StorageResult<()> {
        let connection = provider.connection;
        sql(self.connection.execute("INSERT INTO providers(provider_instance_id, display_name, provider_type, endpoint, api_key, protocol_preference, models_path, discovery_format) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(provider_instance_id) DO UPDATE SET display_name=excluded.display_name, provider_type=excluded.provider_type, endpoint=excluded.endpoint, api_key=excluded.api_key, protocol_preference=excluded.protocol_preference, models_path=excluded.models_path, discovery_format=excluded.discovery_format", params![provider.provider_instance_id.as_str(), connection.display_name, enum_write(&connection.provider_type)?, connection.endpoint, provider.api_key.expose(), enum_write(&connection.protocol_preference)?, connection.models_path, enum_write(&connection.discovery_format)?]))?;
        Ok(())
    }
    /// 统计与删除共用 Immediate 事务，响应准确反映本次删除；模型引用和历史保留。
    pub(super) fn remove_provider(
        &mut self,
        id: ProviderInstanceId,
    ) -> StorageResult<ProviderUsage> {
        let transaction = sql(self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let usage = read_provider_usage(&transaction, &id)?;
        sql(transaction.execute(
            "DELETE FROM providers WHERE provider_instance_id=?1",
            [id.as_str()],
        ))?;
        sql(transaction.commit())?;
        Ok(usage)
    }
    /// 只读事务固定统计与标题的同一快照，不因查询而恢复或装配任何会话。
    pub(super) fn provider_usage(
        &mut self,
        id: ProviderInstanceId,
    ) -> StorageResult<ProviderUsage> {
        let transaction = sql(self.connection.transaction())?;
        let usage = read_provider_usage(&transaction, &id)?;
        sql(transaction.commit())?;
        Ok(usage)
    }
    pub(super) fn load_model_settings(&self) -> StorageResult<ModelSettings> {
        let raw = sql(self.connection.query_row("SELECT default_provider_instance_id, default_model_id, vision_provider_instance_id, vision_model_id FROM model_settings WHERE singleton_key=1", [], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?))))?;
        Ok(ModelSettings {
            default_model: selection(raw.0, raw.1)?,
            vision_model: selection(raw.2, raw.3)?,
        })
    }
    pub(super) fn save_model_settings(&self, settings: ModelSettings) -> StorageResult<()> {
        let rows = sql(self.connection.execute("UPDATE model_settings SET default_provider_instance_id=?1, default_model_id=?2, vision_provider_instance_id=?3, vision_model_id=?4 WHERE singleton_key=1", params![settings.default_model.as_ref().map(|s|s.provider_instance_id.as_str()), settings.default_model.as_ref().map(|s|s.model_id.as_str()), settings.vision_model.as_ref().map(|s|s.provider_instance_id.as_str()), settings.vision_model.as_ref().map(|s|s.model_id.as_str())]))?;
        if rows != 1 {
            return Err(invalid_data("model settings singleton is missing"));
        }
        Ok(())
    }
    pub(super) fn get_model_fixed_config(
        &self,
        selection: ModelSelection,
    ) -> StorageResult<Option<ModelFixedConfig>> {
        let mut statement = sql(self.connection.prepare(&format!("SELECT {FIXED_COLUMNS} FROM model_fixed_configs WHERE provider_instance_id=?1 AND model_id=?2")))?;
        sql(statement
            .query_row(
                params![selection.provider_instance_id.as_str(), selection.model_id],
                read_fixed,
            )
            .optional())?
        .transpose()
    }
    pub(super) fn list_model_fixed_configs(
        &self,
        id: ProviderInstanceId,
        offset: u32,
        limit: u32,
    ) -> StorageResult<Vec<ModelFixedConfig>> {
        let mut statement = sql(self.connection.prepare(&format!("SELECT {FIXED_COLUMNS} FROM model_fixed_configs WHERE provider_instance_id=?1 ORDER BY model_id LIMIT ?2 OFFSET ?3")))?;
        let rows =
            sql(statement.query_map(params![id.as_str(), limit.min(200), offset], read_fixed))?;
        rows.map(|row| sql(row)?).collect()
    }
    pub(super) fn put_model_fixed_config(&self, config: ModelFixedConfig) -> StorageResult<()> {
        if self
            .get_model_fixed_config(config.selection.clone())?
            .is_some_and(|existing| existing.origin != config.origin)
        {
            return Err(invalid_data("model origin cannot change"));
        }
        let (mut capabilities, context, output, input) = split_parameters(config.parameters)?;
        let mut modes = Map::new();
        for name in ["reasoning_max_input_tokens", "reasoning_max_output_tokens"] {
            modes.insert(
                name.into(),
                capabilities
                    .remove(name)
                    .ok_or_else(|| invalid_data("model mode limit is missing"))?,
            );
        }
        sql(self.connection.execute("INSERT INTO model_fixed_configs(provider_instance_id,model_id,context_window_tokens,max_output_tokens,max_input_tokens,mode_limits_json,capabilities_json,updated_at_ms,origin) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(provider_instance_id,model_id) DO UPDATE SET context_window_tokens=excluded.context_window_tokens,max_output_tokens=excluded.max_output_tokens,max_input_tokens=excluded.max_input_tokens,mode_limits_json=excluded.mode_limits_json,capabilities_json=excluded.capabilities_json,updated_at_ms=excluded.updated_at_ms", params![config.selection.provider_instance_id.as_str(),config.selection.model_id,context,output,input,Value::Object(modes).to_string(),Value::Object(capabilities).to_string(),config.updated_at_ms,enum_write(&config.origin)?]))?;
        Ok(())
    }
    pub(super) fn reset_model_fixed_config(&self, selection: ModelSelection) -> StorageResult<()> {
        sql(self.connection.execute(
            "DELETE FROM model_fixed_configs WHERE provider_instance_id=?1 AND model_id=?2",
            params![selection.provider_instance_id.as_str(), selection.model_id],
        ))?;
        Ok(())
    }
}
fn sql<T>(result: rusqlite::Result<T>) -> StorageResult<T> {
    result.map_err(|error| internal_error("model settings storage operation failed", error))
}
fn identifier(raw: String) -> StorageResult<ProviderInstanceId> {
    ProviderInstanceId::new(raw).map_err(|_| invalid_data("stored provider identifier is invalid"))
}
fn selection(
    provider: Option<String>,
    model: Option<String>,
) -> StorageResult<Option<ModelSelection>> {
    match (provider, model) {
        (None, None) => Ok(None),
        (Some(provider), Some(model_id)) if !model_id.trim().is_empty() => {
            Ok(Some(ModelSelection {
                provider_instance_id: identifier(provider)?,
                model_id,
            }))
        }
        _ => Err(invalid_data("stored model selection is invalid")),
    }
}

/// Session 查询把两个实际列投影成临时 JSON 数组以复用同一读取校验；数据库不保存该 JSON。
pub(super) fn read_selection_json(raw: &str) -> StorageResult<Option<ModelSelection>> {
    let (provider, model): (Option<String>, Option<String>) = serde_json::from_str(raw)
        .map_err(|error| internal_error("stored session model reference is invalid", error))?;
    selection(provider, model)
}
fn enum_read<T: DeserializeOwned>(raw: String) -> StorageResult<T> {
    serde_json::from_value(Value::String(raw))
        .map_err(|e| internal_error("stored provider setting is invalid", e))
}
fn enum_write<T: Serialize>(value: &T) -> StorageResult<String> {
    match serde_json::to_value(value)
        .map_err(|e| internal_error("provider setting encoding failed", e))?
    {
        Value::String(value) => Ok(value),
        _ => Err(invalid_data("provider setting must be a string")),
    }
}
type FixedParameterColumns = (Map<String, Value>, i64, i64, Option<i64>);

fn split_parameters(parameters: ModelParameters) -> StorageResult<FixedParameterColumns> {
    assistant_runtime::validate_fixed_model_parameters(&parameters)
        .map_err(|_| invalid_data("fixed model parameters are inconsistent"))?;
    let known = |limit| match limit {
        ModelTokenLimit::Known(value) => i64::try_from(value.get())
            .map_err(|_| invalid_data("fixed model limit is out of range")),
        _ => Err(invalid_data("fixed model requires known positive limits")),
    };
    let context = known(parameters.context_window_tokens)?;
    let output = known(parameters.max_output_tokens)?;
    let input = match parameters.max_input_tokens {
        ModelTokenLimit::Unknown => None,
        value => Some(known(value)?),
    };
    let Value::Object(mut fields) = serde_json::to_value(parameters)
        .map_err(|e| internal_error("fixed model encoding failed", e))?
    else {
        return Err(invalid_data("fixed model must be an object"));
    };
    for name in [
        "context_window_tokens",
        "max_output_tokens",
        "max_input_tokens",
    ] {
        fields.remove(name);
    }
    Ok((fields, context, output, input))
}
fn read_fixed(row: &Row<'_>) -> rusqlite::Result<StorageResult<ModelFixedConfig>> {
    let id: String = row.get(0)?;
    let model_id: String = row.get(1)?;
    let context: i64 = row.get(2)?;
    let output: i64 = row.get(3)?;
    let input: Option<i64> = row.get(4)?;
    let modes: String = row.get(5)?;
    let capabilities: String = row.get(6)?;
    let updated_at_ms: i64 = row.get(7)?;
    Ok((|| {
        let mut fields: Map<String, Value> = serde_json::from_str(&capabilities)
            .map_err(|e| internal_error("stored model capabilities invalid", e))?;
        let modes: Map<String, Value> = serde_json::from_str(&modes)
            .map_err(|e| internal_error("stored mode limits invalid", e))?;
        for (key, value) in modes {
            if fields.insert(key, value).is_some() {
                return Err(invalid_data("duplicate stored model field"));
            }
        }
        for (name, value) in [
            ("context_window_tokens", Some(context)),
            ("max_output_tokens", Some(output)),
            ("max_input_tokens", input),
        ] {
            let limit = match value {
                None => ModelTokenLimit::Unknown,
                Some(value) => ModelTokenLimit::Known(
                    NonZeroU64::new(
                        u64::try_from(value)
                            .map_err(|_| invalid_data("stored model limit is negative"))?,
                    )
                    .ok_or_else(|| invalid_data("stored model limit is zero"))?,
                ),
            };
            if fields
                .insert(
                    name.into(),
                    serde_json::to_value(limit)
                        .map_err(|e| internal_error("stored model limit encoding failed", e))?,
                )
                .is_some()
            {
                return Err(invalid_data("duplicate stored model limit"));
            }
        }
        let parameters = serde_json::from_value(Value::Object(fields))
            .map_err(|e| internal_error("stored fixed parameters are invalid", e))?;
        Ok(ModelFixedConfig {
            origin: enum_read(
                row.get(8)
                    .map_err(|e| internal_error("stored model origin invalid", e))?,
            )?,
            selection: ModelSelection {
                provider_instance_id: identifier(id)?,
                model_id,
            },
            parameters,
            updated_at_ms,
        })
    })())
}

fn read_provider_usage(
    connection: &rusqlite::Connection,
    id: &ProviderInstanceId,
) -> StorageResult<ProviderUsage> {
    let (default_model, vision_model, session_count, fixed_config_count) = sql(connection.query_row(
        "SELECT COALESCE(default_provider_instance_id=?1,0), COALESCE(vision_provider_instance_id=?1,0),
         (SELECT COUNT(*) FROM sessions WHERE model_provider_instance_id=?1),
         (SELECT COUNT(*) FROM model_fixed_configs WHERE provider_instance_id=?1)
         FROM model_settings WHERE singleton_key=1", [id.as_str()], |row| Ok((row.get::<_,bool>(0)?, row.get::<_,bool>(1)?, row.get::<_,i64>(2)?, row.get::<_,i64>(3)?))
    ))?;
    let mut statement = sql(connection.prepare("SELECT session_id, title FROM sessions WHERE model_provider_instance_id=?1 ORDER BY session_id LIMIT 20"))?;
    let sessions = sql(statement.query_map([id.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }))?
    .map(|row| {
        let (id, title) = sql(row)?;
        Ok(ProviderSessionUsage {
            session_id: SessionId::new(id)
                .map_err(|_| invalid_data("stored session identifier is invalid"))?,
            title,
        })
    })
    .collect::<StorageResult<Vec<_>>>()?;
    Ok(ProviderUsage {
        default_model,
        vision_model,
        session_count: u64::try_from(session_count)
            .map_err(|_| invalid_data("invalid provider session count"))?,
        fixed_config_count: u64::try_from(fixed_config_count)
            .map_err(|_| invalid_data("invalid provider configuration count"))?,
        sessions,
    })
}
