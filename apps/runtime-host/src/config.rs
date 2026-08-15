//! Runtime Host 的非敏感 bootstrap 参数与 Runtime Home 路径解析。

use std::{ffi::OsString, num::NonZeroUsize, path::PathBuf};

use clap::{Parser, Subcommand};
use thiserror::Error;

const DEFAULT_RUNTIME_HOME_DIRECTORY: &str = ".ez-assistant";
const CONFIG_FILE: &str = "config.toml";
const DEFAULT_EVENT_CAPACITY: usize = 256;

/// Runtime Host 的进程级命令行入口。
#[derive(Debug, Parser)]
#[command(
    name = "ez-assistant-runtime",
    version,
    about = "EZ Assistant Runtime Host",
    after_help = "The Host listens on a dynamic IPv4 loopback HTTP port and publishes private discovery data under Runtime Home/run/runtime.json."
)]
struct Cli {
    #[command(subcommand)]
    action: CliAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub(crate) enum CliAction {
    /// Start an independent Runtime Host process and return after it is spawned.
    Launch(LaunchArguments),
    /// Start the Runtime Host.
    Serve(ServeArguments),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct LaunchArguments {
    /// Absolute Runtime Home override; defaults to ~/.ez-assistant.
    #[arg(long, value_name = "PATH")]
    runtime_home: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct ServeArguments {
    /// Absolute Runtime Home override; defaults to ~/.ez-assistant.
    #[arg(long, value_name = "PATH")]
    runtime_home: Option<PathBuf>,
    /// Positive Runtime event buffer capacity; defaults to 256.
    #[arg(long, value_name = "COUNT")]
    event_capacity: Option<NonZeroUsize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeConfig {
    pub(crate) runtime_home: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) event_capacity: NonZeroUsize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchConfig {
    pub(crate) runtime_home: PathBuf,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum ConfigError {
    #[error("the user home directory is unavailable; use --runtime-home")]
    HomeDirectoryUnavailable,
    #[error("Runtime Home must be absolute")]
    RelativeRuntimeHome,
}

pub(crate) fn parse_cli(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<CliAction, clap::Error> {
    let mut arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.is_empty() {
        arguments.push(OsString::from("--help"));
    }
    let arguments = std::iter::once(OsString::from("ez-assistant-runtime")).chain(arguments);
    Cli::try_parse_from(arguments).map(|cli| cli.action)
}

impl ServeConfig {
    pub(crate) fn resolve(arguments: ServeArguments) -> Result<Self, ConfigError> {
        let runtime_home = resolve_runtime_home(arguments.runtime_home)?;
        Ok(Self {
            config_path: runtime_home.join(CONFIG_FILE),
            runtime_home,
            event_capacity: arguments.event_capacity.unwrap_or_else(|| {
                NonZeroUsize::new(DEFAULT_EVENT_CAPACITY).expect("static capacity is non-zero")
            }),
        })
    }
}

impl LaunchConfig {
    pub(crate) fn resolve(arguments: LaunchArguments) -> Result<Self, ConfigError> {
        Ok(Self {
            runtime_home: resolve_runtime_home(arguments.runtime_home)?,
        })
    }
}

fn resolve_runtime_home(override_path: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    let path = match override_path {
        Some(path) => path,
        None => default_runtime_home(dirs::home_dir())?,
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ConfigError::RelativeRuntimeHome)
    }
}

/// 只在 bootstrap 边界把用户目录解析为绝对路径；后续始终持有完整路径。
fn default_runtime_home(home: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    home.map(|path| path.join(DEFAULT_RUNTIME_HOME_DIRECTORY))
        .ok_or(ConfigError::HomeDirectoryUnavailable)
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn expect_serve(action: CliAction) -> ServeArguments {
        match action {
            CliAction::Serve(arguments) => arguments,
            CliAction::Launch(_) => panic!("expected serve action"),
        }
    }

    fn expect_launch(action: CliAction) -> LaunchArguments {
        match action {
            CliAction::Launch(arguments) => arguments,
            CliAction::Serve(_) => panic!("expected launch action"),
        }
    }

    #[test]
    fn clap_generates_help_and_rejects_unknown_subcommands() {
        assert_eq!(
            parse_cli(Vec::<OsString>::new())
                .expect_err("missing command")
                .kind(),
            ErrorKind::DisplayHelp
        );
        assert_eq!(
            parse_cli([OsString::from("unknown")])
                .expect_err("unknown")
                .kind(),
            ErrorKind::InvalidSubcommand
        );
    }

    #[test]
    fn serve_requires_an_absolute_runtime_home_and_positive_capacity() {
        assert_eq!(
            parse_cli([
                OsString::from("serve"),
                OsString::from("--event-capacity"),
                OsString::from("0"),
            ])
            .expect_err("zero capacity")
            .kind(),
            ErrorKind::ValueValidation
        );
        let arguments = expect_serve(
            parse_cli([
                OsString::from("serve"),
                OsString::from("--runtime-home"),
                OsString::from("relative"),
            ])
            .expect("parse"),
        );
        assert_eq!(
            ServeConfig::resolve(arguments),
            Err(ConfigError::RelativeRuntimeHome)
        );
    }

    #[test]
    fn serve_derives_config_from_runtime_home() {
        let home = std::env::temp_dir().join("runtime-host-config-test");
        let arguments = expect_serve(
            parse_cli([
                OsString::from("serve"),
                OsString::from("--runtime-home"),
                home.clone().into_os_string(),
            ])
            .expect("parse"),
        );
        let config = ServeConfig::resolve(arguments).expect("config");
        assert_eq!(config.runtime_home, home);
        assert_eq!(config.config_path, home.join(CONFIG_FILE));
    }

    #[test]
    fn launch_uses_the_same_runtime_home_contract_as_serve() {
        let home = std::env::temp_dir().join("runtime-host-launch-test");
        let arguments = expect_launch(
            parse_cli([
                OsString::from("launch"),
                OsString::from("--runtime-home"),
                home.clone().into_os_string(),
            ])
            .expect("parse"),
        );
        assert_eq!(
            LaunchConfig::resolve(arguments),
            Ok(LaunchConfig { runtime_home: home })
        );
    }

    #[test]
    fn default_runtime_home_is_a_single_user_visible_root() {
        let home = PathBuf::from("/Users/example");
        assert_eq!(
            default_runtime_home(Some(home.clone())),
            Ok(home.join(".ez-assistant"))
        );
        assert_eq!(
            default_runtime_home(None),
            Err(ConfigError::HomeDirectoryUnavailable)
        );
    }

    #[test]
    fn private_web_demo_startup_switch_is_not_part_of_the_product_host() {
        assert_eq!(
            parse_cli([OsString::from("serve"), OsString::from("--web-demo"),])
                .expect_err("feature-disabled switch")
                .kind(),
            ErrorKind::UnknownArgument
        );
    }
}
