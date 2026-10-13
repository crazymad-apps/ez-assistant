//! 连接续接策略沿用 providers；新状态只能由本版 Host 消费，下限与结构同事务提交。
use super::{Migration, Result, v0_28_1};
use rusqlite::Connection;

mod shell_choices;

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.29.0",
        min_compatible_host_version: Some("0.29.0"),
        rebuilds_referenced_tables: false,
        validate_source: v0_28_1::validate,
        apply,
        validate,
    }
}
fn apply(connection: &Connection) -> Result<()> {
    shell_choices::apply(connection)?;
    connection.execute_batch("ALTER TABLE sessions ADD COLUMN server_history_key TEXT CHECK(server_history_key IS NULL OR length(server_history_key)>0);
        ALTER TABLE providers ADD COLUMN server_history_enabled INTEGER NOT NULL DEFAULT 0 CHECK(server_history_enabled IN (0,1));
        ALTER TABLE providers ADD COLUMN server_history_support_override TEXT CHECK(server_history_support_override IN ('supported','unsupported'));
        ALTER TABLE providers ADD COLUMN server_history_key TEXT CHECK((server_history_enabled=0 AND server_history_key IS NULL) OR (server_history_enabled=1 AND server_history_key IS NOT NULL AND length(server_history_key)>0));")?;
    Ok(())
}
pub(super) fn validate(connection: &Connection) -> Result<()> {
    validate_layout(connection)?;
    validate_history_policy(connection)
}

// 后继版本复用既有布局核验，但由各自版本校验演进后的配置周期约束。
pub(super) fn validate_layout(connection: &Connection) -> Result<()> {
    v0_28_1::validate(connection)?;
    shell_choices::validate(connection)?;
    connection.prepare("SELECT server_history_key FROM sessions LIMIT 0")?;
    connection.prepare("SELECT server_history_enabled, server_history_support_override, server_history_key FROM providers LIMIT 0")?;
    Ok(())
}

fn validate_history_policy(connection: &Connection) -> Result<()> {
    let invalid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE server_history_key IS NOT NULL AND length(server_history_key)=0) OR EXISTS(SELECT 1 FROM providers WHERE server_history_enabled NOT IN (0,1) OR (server_history_enabled=0 AND server_history_key IS NOT NULL) OR (server_history_enabled=1 AND (server_history_key IS NULL OR length(server_history_key)=0)) OR (server_history_support_override IS NOT NULL AND server_history_support_override NOT IN ('supported','unsupported')))", [], |row| row.get(0))?;
    if invalid {
        return Err(super::MigrationError::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
