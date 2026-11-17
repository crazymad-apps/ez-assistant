//! 仅追加侧栏排序字段；既有正文格式、代次与接纳身份保持原样。

use rusqlite::Connection;

use super::super::{Migration, MigrationError, Result};

pub(super) const KEY: &str = "sidebar_order_v1";

pub(in crate::storage::migrations) fn entry() -> Migration {
    Migration {
        key: KEY,
        version: "0.33.1",
        min_compatible_host_version: Some("0.33.1"),
        rebuilds_referenced_tables: false,
        validate_source: super::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    // 升级时必须有完整分组来源；日常已对齐库仍沿原按需恢复补行，不由校验器代写。
    if connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions s LEFT JOIN session_resources r USING(session_id) WHERE r.session_id IS NULL)", [], |row| row.get::<_, bool>(0))? {
        return Err(MigrationError::Integrity);
    }
    connection.execute_batch(include_str!("sidebar_order.sql"))?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    super::validate(connection)?;
    // 启动只核验，不借缺行、重复顺序或错误存储类型静默修复已有数据。
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM workspaces WHERE typeof(sort_order) != 'integer')",
        "SELECT EXISTS(SELECT 1 FROM session_resources WHERE typeof(sort_order) != 'integer')",
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE typeof(pin_order) != 'integer')",
        "SELECT EXISTS(SELECT 1 FROM workspaces GROUP BY sort_order HAVING COUNT(*) > 1)",
        "SELECT EXISTS(SELECT 1 FROM session_resources GROUP BY workspace_id, sort_order HAVING COUNT(*) > 1)",
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE is_pinned = 1 GROUP BY pin_order HAVING COUNT(*) > 1)",
    ] {
        if connection.query_row(sql, [], |row| row.get::<_, bool>(0))? {
            return Err(MigrationError::Integrity);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
