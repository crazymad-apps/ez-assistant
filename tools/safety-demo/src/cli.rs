//! Safety Demo 命令行入口。

pub(crate) use crate::config::{CliAction, ConfigError, HELP};

pub(crate) fn parse() -> Result<CliAction, ConfigError> {
    crate::config::parse_env()
}
