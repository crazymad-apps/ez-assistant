//! 用户域目录迁移。定位字段与本版账本/最低 Host 在控制器的同一事务中提交。

use super::{Migration, MigrationError, Result, v0_26_0};
#[cfg(test)]
mod tests;
use crate::host_layout::paths::Relocation;
use rusqlite::Connection;

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.27.0",
        min_compatible_host_version: Some("0.27.0"),
        validate_source: v0_26_0::validate,
        apply,
        validate,
    }
}

/// 冻结实际子模型窗口，旧任务保持未知，不从当前配置回填。
fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE child_tasks ADD COLUMN context_window_tokens INTEGER
        CHECK(context_window_tokens IS NULL OR (typeof(context_window_tokens)='integer' AND context_window_tokens>0));")?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    v0_26_0::validate(connection)?;
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM child_tasks WHERE context_window_tokens IS NOT NULL AND (typeof(context_window_tokens)!='integer' OR context_window_tokens<=0))",
        [], |row| row.get(0),
    )?;
    if invalid {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

/// 仅布局升级传入映射；新用户库只执行原版本链。没有全文 JSON 或历史正文替换。
pub(super) fn relocate(connection: &Connection, paths: &Relocation) -> Result<()> {
    for (table, column, array) in [
        ("workspaces", "user_directory", false),
        ("workspaces", "agent_directory", false),
        ("workspaces", "additional_directories_json", true),
        ("session_resources", "working_directory", false),
        ("session_resources", "private_directory", false),
        ("session_resources", "attachment_directory", false),
        (
            "session_resources",
            "additional_workspace_directories_json",
            true,
        ),
        ("attachments", "agent_readable_path", false),
    ] {
        let rows = connection
            .prepare(&format!("SELECT rowid, {column} FROM {table}"))?
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, original) in rows {
            let changed = if array {
                let values: Vec<String> = serde_json::from_str(&original)?;
                let mapped: Vec<_> = values.iter().map(|v| paths.text(v)).collect();
                if values == mapped {
                    original.clone()
                } else {
                    serde_json::to_string(&mapped)?
                }
            } else {
                paths.text(&original)
            };
            if changed != original {
                let affected = connection.execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2 AND {column}=?3"),
                    rusqlite::params![changed, id, original],
                )?;
                if affected != 1 {
                    return Err(MigrationError::DatabaseChanged);
                }
            }
        }
    }
    // 先修改附件当前定位，再按附件归属校验 queued FileReferences 的唯一映射。
    let rows = connection.prepare("SELECT input_id, session_id, queued_message_json FROM inputs WHERE queued_message_json IS NOT NULL")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, session, original) in rows {
        let mut message: agent_types::UserMessage = serde_json::from_str(&original)?;
        let mut changed = false;
        for part in &mut message.parts {
            if let agent_types::UserPart::FileReferences(references) = part {
                for file in &mut references.files {
                    let mapped = paths.text(&file.readable_path);
                    if mapped != file.readable_path {
                        let count: i64 = connection.query_row("SELECT COUNT(*) FROM attachments WHERE session_id=?1 AND agent_readable_path=?2 AND original_name=?3", rusqlite::params![session, mapped, file.original_name], |r| r.get(0))?;
                        if count != 1 {
                            return Err(MigrationError::Integrity);
                        }
                        file.readable_path = mapped;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            connection.execute(
                "UPDATE inputs SET queued_message_json=?1 WHERE input_id=?2",
                rusqlite::params![serde_json::to_string(&message)?, id],
            )?;
        }
    }
    Ok(())
}
