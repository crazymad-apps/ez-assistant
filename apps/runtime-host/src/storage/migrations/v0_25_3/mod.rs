//! Shell 绑定、全局默认和 Run 冻结事实；由外层统一备份并在单事务内应用。

use rusqlite::Connection;

use super::{Migration, MigrationError, Result, v0_25_1};

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.25.3",
        // 老 Host 不认识绑定，会用不同解释器执行命令，不能继续写入此库。
        min_compatible_host_version: Some("0.25.3"),
        validate_source: v0_25_1::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE sessions ADD COLUMN agent_shell_kind TEXT
           CHECK(agent_shell_kind IN ('windows_powershell_51','cmd','powershell_7','git_bash','posix_sh'));
         ALTER TABLE inputs ADD COLUMN agent_shell_target TEXT
           CHECK(agent_shell_target IS NULL OR (input_kind='message' AND agent_shell_target IN ('windows_powershell_51','cmd','powershell_7','git_bash','posix_sh')));
         ALTER TABLE runs ADD COLUMN shell_snapshot_json TEXT
           CHECK(shell_snapshot_json IS NULL OR json_valid(shell_snapshot_json));
         ALTER TABLE sessions ADD COLUMN agent_shell_environment_json TEXT
           CHECK(agent_shell_environment_json IS NULL OR json_valid(agent_shell_environment_json));
         CREATE TABLE agent_shell_settings (
           singleton_key INTEGER PRIMARY KEY CHECK(singleton_key=1),
           default_agent_shell_kind TEXT
             CHECK(default_agent_shell_kind IN ('windows_powershell_51','cmd','powershell_7','git_bash','posix_sh'))
         );
         INSERT INTO agent_shell_settings(singleton_key, default_agent_shell_kind) VALUES(1,NULL);",
    )?;
    Ok(())
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    v0_25_1::validate(connection)?;
    connection.prepare("SELECT agent_shell_kind FROM sessions LIMIT 0")?;
    connection.prepare("SELECT agent_shell_environment_json FROM sessions LIMIT 0")?;
    connection.prepare("SELECT agent_shell_target FROM inputs LIMIT 0")?;
    connection.prepare("SELECT shell_snapshot_json FROM runs LIMIT 0")?;
    let settings: i64 =
        connection.query_row("SELECT COUNT(*) FROM agent_shell_settings", [], |row| {
            row.get(0)
        })?;
    if settings != 1 {
        return Err(MigrationError::Integrity);
    }
    let key: i64 = connection.query_row(
        "SELECT singleton_key FROM agent_shell_settings",
        [],
        |row| row.get(0),
    )?;
    if key != 1 {
        return Err(MigrationError::Integrity);
    }
    for query in [
        "SELECT agent_shell_kind FROM sessions WHERE agent_shell_kind IS NOT NULL",
        "SELECT agent_shell_target FROM inputs WHERE agent_shell_target IS NOT NULL",
        "SELECT default_agent_shell_kind FROM agent_shell_settings WHERE default_agent_shell_kind IS NOT NULL",
    ] {
        let mut statement = connection.prepare(query)?;
        for value in statement.query_map([], |row| row.get::<_, String>(0))? {
            let value = serde_json::Value::String(value?);
            serde_json::from_value::<assistant_protocol::ShellKind>(value)
                .map_err(|_| MigrationError::Integrity)?;
        }
    }
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM inputs WHERE agent_shell_target IS NOT NULL AND input_kind!='message')",
        [], |row| row.get(0),
    )?;
    if invalid {
        return Err(MigrationError::Integrity);
    }
    let mut statement = connection
        .prepare("SELECT shell_snapshot_json FROM runs WHERE shell_snapshot_json IS NOT NULL")?;
    for value in statement.query_map([], |row| row.get::<_, String>(0))? {
        serde_json::from_str::<assistant_runtime::FrozenShellEnvironment>(&value?)
            .map_err(|_| MigrationError::Integrity)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
