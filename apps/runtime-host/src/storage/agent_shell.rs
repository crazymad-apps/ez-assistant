//! 全局默认只在新 Session 初始化时读取；已有 Session 的绑定由领取事务修改。

use assistant_protocol::ShellKind;

use super::{StorageEngine, StorageResult, database_write_error, internal_error, mode};

pub(super) fn encode_environment(
    environment: Option<&assistant_runtime::FrozenShellEnvironment>,
) -> StorageResult<Option<String>> {
    environment
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| super::invalid_data_with_source("shell environment is invalid", source))
}

impl StorageEngine {
    pub(super) fn load_default_agent_shell(&self) -> StorageResult<Option<ShellKind>> {
        let value = self
            .connection
            .query_row(
                "SELECT default_agent_shell_kind FROM agent_shell_settings WHERE singleton_key=1",
                [],
                |row| row.get(0),
            )
            .map_err(|source| internal_error("default agent shell could not be read", source))?;
        mode::parse_shell_kind(value)
    }

    pub(super) fn save_default_agent_shell(&mut self, kind: ShellKind) -> StorageResult<()> {
        let changed = self
            .connection
            .execute(
                "UPDATE agent_shell_settings SET default_agent_shell_kind=?1 WHERE singleton_key=1",
                [mode::shell_kind_value(kind)],
            )
            .map_err(|source| {
                database_write_error("default agent shell could not be saved", source)
            })?;
        if changed != 1 {
            return Err(super::invalid_data("default agent shell row is missing"));
        }
        Ok(())
    }
}
