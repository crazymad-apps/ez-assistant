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
    /// Start the Runtime Host.
    Serve(ServeArguments),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct ServeArguments {
    /// Absolute Runtime Home override; defaults to ~/.ez-assistant.
    #[arg(long, value_name = "PATH")]
    runtime_home: Option<PathBuf>,
    /// Positive Runtime event buffer capacity; defaults to 256.
    #[arg(long, value_name = "COUNT")]
    event_capacity: Option<NonZeroUsize>,
    /// UNSAFE: enable unrestricted local file and shell tools. The Workspace is not a sandbox; use only with isolated test data.
    #[arg(long)]
    unsafe_unrestricted_local_tools: bool,
    /// Serve the private browser validation page from this Host instance.
    #[cfg(feature = "web-demo")]
    #[arg(long)]
    web_demo: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeConfig {
    pub(crate) runtime_home: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) event_capacity: NonZeroUsize,
    pub(crate) unsafe_unrestricted_local_tools: bool,
    pub(crate) web_demo: bool,
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
            unsafe_unrestricted_local_tools: arguments.unsafe_unrestricted_local_tools,
            #[cfg(feature = "web-demo")]
            web_demo: arguments.web_demo,
            #[cfg(not(feature = "web-demo"))]
            web_demo: false,
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
        let CliAction::Serve(arguments) = action;
        arguments
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
        assert!(!config.unsafe_unrestricted_local_tools);
        assert!(!config.web_demo);
    }

    #[test]
    fn unrestricted_local_tools_require_the_explicit_risk_named_switch() {
        let home = std::env::temp_dir().join("runtime-host-config-test");
        let arguments = expect_serve(
            parse_cli([
                OsString::from("serve"),
                OsString::from("--runtime-home"),
                home.into_os_string(),
                OsString::from("--unsafe-unrestricted-local-tools"),
            ])
            .expect("parse"),
        );
        assert!(
            ServeConfig::resolve(arguments)
                .expect("config")
                .unsafe_unrestricted_local_tools
        );
    }

    #[test]
    fn unrestricted_switch_help_states_that_workspace_is_not_a_sandbox() {
        let error = parse_cli([OsString::from("serve"), OsString::from("--help")])
            .expect_err("help exits through clap");
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("--unsafe-unrestricted-local-tools"));
        assert!(help.contains("Workspace is not a sandbox"));
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

    #[cfg(feature = "web-demo")]
    #[test]
    fn web_demo_still_requires_an_explicit_startup_switch() {
        let home = std::env::temp_dir().join("runtime-host-config-test");
        let arguments = expect_serve(
            parse_cli([
                OsString::from("serve"),
                OsString::from("--runtime-home"),
                home.into_os_string(),
                OsString::from("--web-demo"),
            ])
            .expect("parse"),
        );
        assert!(ServeConfig::resolve(arguments).expect("config").web_demo);
    }

    #[cfg(not(feature = "web-demo"))]
    #[test]
    fn builds_without_web_demo_do_not_accept_the_startup_switch() {
        assert_eq!(
            parse_cli([OsString::from("serve"), OsString::from("--web-demo"),])
                .expect_err("feature-disabled switch")
                .kind(),
            ErrorKind::UnknownArgument
        );
    }
}
