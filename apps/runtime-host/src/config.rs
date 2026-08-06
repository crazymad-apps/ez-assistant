//! Runtime Host 的显式 CLI 与环境配置。

use std::{
    ffi::OsString,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use thiserror::Error;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-chat";
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;
const DEFAULT_EVENT_CAPACITY: usize = 256;
const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const SOCKET_FILE: &str = "runtime.sock";

/// Runtime Host 的进程级命令行入口。
#[derive(Debug, Parser)]
#[command(
    name = "ez-assistant-runtime",
    version,
    about = "EZ Assistant Runtime Host",
    after_help = "The Host listens only on a Unix domain socket. In Demo, `Q` exits the client only; `Ctrl+Q` requests controlled Runtime shutdown after confirmation."
)]
struct Cli {
    #[command(subcommand)]
    action: CliAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub(crate) enum CliAction {
    /// Start the Runtime Host.
    Serve(ServeArguments),
    /// Start the private validation client.
    #[cfg(feature = "demo-client")]
    Demo(DemoArguments),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct ServeArguments {
    /// Runtime directory; defaults to OS temp/ez-assistant-runtime.
    #[arg(long, value_name = "PATH")]
    runtime_dir: Option<PathBuf>,
    /// Explicit Unix socket path; overrides --runtime-dir.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Provider base URL; falls back to DEEPSEEK_BASE_URL, then <https://api.deepseek.com>.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Provider model; falls back to DEEPSEEK_MODEL, then deepseek-chat.
    #[arg(long, value_name = "NAME")]
    model: Option<String>,
    /// Positive context-window tokens; falls back to DEEPSEEK_CONTEXT_WINDOW_TOKENS, then 128000.
    #[arg(long, value_name = "TOKENS")]
    context_window: Option<NonZeroU64>,
    /// Positive Runtime event buffer capacity; defaults to 256.
    #[arg(long, value_name = "COUNT")]
    event_capacity: Option<NonZeroUsize>,
    /// Provider credential variable name; defaults to DEEPSEEK_API_KEY.
    #[arg(long, value_name = "NAME")]
    api_key_env: Option<String>,
}

#[cfg(feature = "demo-client")]
#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct DemoArguments {
    /// Must resolve to the Host runtime directory.
    #[arg(long, value_name = "PATH")]
    runtime_dir: Option<PathBuf>,
    /// Explicit Unix socket path.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeConfig {
    pub(crate) socket_path: PathBuf,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) context_window_tokens: u64,
    pub(crate) event_capacity: NonZeroUsize,
    pub(crate) api_key_env: String,
}

#[cfg(feature = "demo-client")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DemoConfig {
    pub(crate) socket_path: PathBuf,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum ConfigError {
    #[error("environment variable `{0}` must be a positive integer")]
    InvalidEnvironmentInteger(&'static str),
    #[error("runtime socket path must be absolute")]
    RelativeSocketPath,
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
        let socket_path = resolve_socket(arguments.runtime_dir, arguments.socket)?;
        let base_url = arguments
            .base_url
            .or_else(|| optional_env("DEEPSEEK_BASE_URL"))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let model = arguments
            .model
            .or_else(|| optional_env("DEEPSEEK_MODEL"))
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let context_window_tokens = match arguments.context_window {
            Some(value) => value.get(),
            None => optional_env("DEEPSEEK_CONTEXT_WINDOW_TOKENS")
                .map(|value| {
                    value
                        .parse::<NonZeroU64>()
                        .map(NonZeroU64::get)
                        .map_err(|_| {
                            ConfigError::InvalidEnvironmentInteger("DEEPSEEK_CONTEXT_WINDOW_TOKENS")
                        })
                })
                .transpose()?
                .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        };
        Ok(Self {
            socket_path,
            base_url,
            model,
            context_window_tokens,
            event_capacity: arguments.event_capacity.unwrap_or_else(|| {
                NonZeroUsize::new(DEFAULT_EVENT_CAPACITY).expect("static capacity is non-zero")
            }),
            api_key_env: arguments
                .api_key_env
                .unwrap_or_else(|| DEFAULT_API_KEY_ENV.to_owned()),
        })
    }
}

#[cfg(feature = "demo-client")]
impl DemoConfig {
    pub(crate) fn resolve(arguments: DemoArguments) -> Result<Self, ConfigError> {
        Ok(Self {
            socket_path: resolve_socket(arguments.runtime_dir, arguments.socket)?,
        })
    }
}

fn resolve_socket(
    runtime_dir: Option<PathBuf>,
    socket: Option<PathBuf>,
) -> Result<PathBuf, ConfigError> {
    let path = socket.unwrap_or_else(|| {
        runtime_dir
            .unwrap_or_else(|| std::env::temp_dir().join("ez-assistant-runtime"))
            .join(SOCKET_FILE)
    });
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ConfigError::RelativeSocketPath)
    }
}

fn optional_env(name: &'static str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn clap_generates_help_and_rejects_unknown_subcommands() {
        assert_eq!(
            parse_cli(Vec::<OsString>::new())
                .expect_err("missing command")
                .kind(),
            ErrorKind::DisplayHelp
        );
        assert_eq!(
            parse_cli([OsString::from("--help")])
                .expect_err("help")
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
    fn clap_requires_positive_numbers_and_config_requires_absolute_socket() {
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
        let action = parse_cli([
            OsString::from("serve"),
            OsString::from("--socket"),
            OsString::from("relative.sock"),
        ])
        .expect("parse");
        let arguments = match action {
            CliAction::Serve(arguments) => arguments,
            #[cfg(feature = "demo-client")]
            CliAction::Demo(_) => panic!("serve action"),
        };
        assert_eq!(
            ServeConfig::resolve(arguments),
            Err(ConfigError::RelativeSocketPath)
        );
    }

    #[cfg(feature = "demo-client")]
    #[test]
    fn demo_and_serve_resolve_the_same_explicit_socket() {
        let path = std::env::temp_dir().join("runtime-host-config-test.sock");
        let serve = parse_cli([
            OsString::from("serve"),
            OsString::from("--socket"),
            path.clone().into_os_string(),
        ])
        .expect("serve");
        let demo = parse_cli([
            OsString::from("demo"),
            OsString::from("--socket"),
            path.clone().into_os_string(),
        ])
        .expect("demo");
        let CliAction::Serve(serve) = serve else {
            panic!("serve action");
        };
        let CliAction::Demo(demo) = demo else {
            panic!("demo action");
        };
        assert_eq!(
            ServeConfig::resolve(serve).expect("serve").socket_path,
            path
        );
        assert_eq!(DemoConfig::resolve(demo).expect("demo").socket_path, path);
    }
}
