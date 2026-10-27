//! 唯一机制与状态用途升级。由迁移控制器持有事务；历史会话固定本地机制，不能从当前配置猜测。
use super::{Migration, MigrationError, Result, v0_29_0};
use rusqlite::Connection;

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.30.0",
        min_compatible_host_version: Some("0.30.0"),
        rebuilds_referenced_tables: true,
        validate_source: v0_29_0::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE sessions ADD COLUMN context_management_mode TEXT CHECK(context_management_mode IN ('local_summary','server_compaction'));
        UPDATE sessions SET context_management_mode='local_summary';
        ALTER TABLE child_tasks ADD COLUMN context_management_mode TEXT NOT NULL DEFAULT 'local_summary' CHECK(context_management_mode IN ('local_summary','server_compaction'));")?;
    // 配置周期同时隔离原生状态；关闭响应续接仍须保留周期。控制器已关闭外键并在提交前核验。
    let original: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='providers'",
        [],
        |row| row.get(0),
    )?;
    let old = "(server_history_enabled=0 AND server_history_key IS NULL) OR (server_history_enabled=1 AND server_history_key IS NOT NULL AND length(server_history_key)>0)";
    if !original.contains(old) {
        return Err(MigrationError::Integrity);
    }
    let target = original
        .replacen("providers", "providers_v0_30", 1)
        .replace(
            old,
            "(server_history_key IS NULL AND server_history_enabled=0) OR (server_history_key IS NOT NULL AND length(server_history_key)>0)",
        );
    let indexes: Vec<String> = connection.prepare("SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name='providers' AND sql IS NOT NULL")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    // 旧空周期先填充到新表投影；旧表仍受 v0.29.0 CHECK 保护，不原位修改其策略。
    connection.execute_batch(&target)?;
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('providers')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let projection: Vec<String> = columns
        .iter()
        .map(|column| {
            if column == "server_history_key" {
                "COALESCE(server_history_key,lower(hex(randomblob(16))))".to_owned()
            } else {
                format!("\"{column}\"")
            }
        })
        .collect();
    connection.execute_batch(&format!("INSERT INTO providers_v0_30 SELECT {} FROM providers; DROP TABLE providers; ALTER TABLE providers_v0_30 RENAME TO providers;", projection.join(",")))?;
    for index in indexes {
        connection.execute_batch(&index)?;
    }
    // 本版本只扩展既有账本 CHECK；保留全部列、记录、复合主键和索引，不新增费用账本。
    let original: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='model_request_records'",
        [],
        |row| row.get(0),
    )?;
    let mut target = original.replacen("model_request_records", "model_request_records_v0_30", 1);
    let old = "'agent_turn', 'context_summary', 'legacy_compacted'";
    if !target.contains(old) {
        return Err(MigrationError::Integrity);
    }
    target = target.replace(
        old,
        "'agent_turn', 'context_summary', 'context_compaction', 'legacy_compacted'",
    );
    let indexes: Vec<String> = connection.prepare("SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name='model_request_records' AND sql IS NOT NULL")?.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    connection.execute_batch(&target)?;
    connection.execute_batch(
        "INSERT INTO model_request_records_v0_30 SELECT * FROM model_request_records;
        DROP TABLE model_request_records;
        ALTER TABLE model_request_records_v0_30 RENAME TO model_request_records;",
    )?;
    for index in indexes {
        connection.execute_batch(&index)?;
    }
    Ok(())
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    v0_29_0::validate_layout(connection)?;
    validate_additions(connection)
}

pub(super) fn validate_additions(connection: &Connection) -> Result<()> {
    connection.prepare("SELECT context_management_mode FROM sessions LIMIT 0")?;
    connection.prepare("SELECT context_management_mode FROM child_tasks LIMIT 0")?;
    let invalid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE context_management_mode IS NOT NULL AND context_management_mode NOT IN ('local_summary','server_compaction')) OR EXISTS(SELECT 1 FROM child_tasks WHERE context_management_mode NOT IN ('local_summary','server_compaction')) OR EXISTS(SELECT 1 FROM model_request_records WHERE request_kind NOT IN ('agent_turn','context_summary','context_compaction','legacy_compacted'))", [], |row| row.get(0))?;
    let invalid_provider: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE server_history_key IS NOT NULL AND length(server_history_key)=0) OR EXISTS(SELECT 1 FROM providers WHERE server_history_enabled NOT IN (0,1) OR (server_history_enabled=1 AND server_history_key IS NULL) OR (server_history_key IS NOT NULL AND length(server_history_key)=0) OR (server_history_support_override IS NOT NULL AND server_history_support_override NOT IN ('supported','unsupported')))", [], |row| row.get(0))?;
    if invalid || invalid_provider {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
