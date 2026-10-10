//! 在既有字段上扩展白名单，逐列拷贝核验；由版本执行器统一备份并包在事务中。
use rusqlite::Connection;

use super::super::{MigrationError, Result};

const COLUMNS: [(&str, &str); 3] = [
    ("sessions", "agent_shell_kind"),
    ("inputs", "agent_shell_target"),
    ("agent_shell_settings", "default_agent_shell_kind"),
];

pub(super) fn apply(connection: &Connection) -> Result<()> {
    for (table, column) in COLUMNS {
        let before: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        let kind = "f02_shell_kind";
        let allowed = format!(
            "{kind} IN ('windows_powershell_51','cmd','powershell_7','git_bash','posix_sh','bash','zsh')"
        );
        let constraint = if table == "inputs" {
            format!("{kind} IS NULL OR (input_kind='message' AND {allowed})")
        } else {
            allowed
        };
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {kind} TEXT CHECK({constraint});
             UPDATE {table} SET {kind}={column};"
        ))?;
        let (copied, differs): (i64, bool) = connection.query_row(
            &format!("SELECT COUNT(*), EXISTS(SELECT 1 FROM {table} WHERE {kind} IS NOT {column}) FROM {table}"),
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if copied != before || differs {
            return Err(MigrationError::Integrity);
        }
        connection.execute_batch(&format!(
            "ALTER TABLE {table} DROP COLUMN {column};
             ALTER TABLE {table} RENAME COLUMN {kind} TO {column};"
        ))?;
        let after: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if after != before {
            return Err(MigrationError::Integrity);
        }
    }
    validate(connection)
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    // 同版在途旧结构明确拒绝，不能让界面能选但在保存时触发旧 CHECK。
    for (table, _) in COLUMNS {
        let ddl: String = connection.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        if !ddl.contains("'bash'") || !ddl.contains("'zsh'") {
            return Err(MigrationError::Integrity);
        }
    }
    Ok(())
}
