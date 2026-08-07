//! Runtime Host 的非敏感 bootstrap 参数与 Runtime Home 路径解析。

use std::{ffi::OsString, num::NonZeroUsize, path::PathBuf};

use clap::{Parser, Subcommand};
use thiserror::Error;

const DEFAULT_RUNTIME_HOME_DIRECTORY: &str = ".ez-assistant";
const CONFIG_FILE: &str = "config.toml";
const RUN_DIRECTORY: &str = "run";
const SOCKET_FILE: &str = "runtime.sock";
const DEFAULT_EVENT_CAPACITY: usize = 256;
/// `sockaddr_un.sun_path` 在目标 Unix 平台上的保守可用字节数（包含末尾 NUL）。
const MAX_SOCKET_PATH_BYTES_WITH_NUL: usize = 104;

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
    /// Absolute Runtime Home override; defaults to ~/.ez-assistant.
    #[arg(long, value_name = "PATH")]
    runtime_home: Option<PathBuf>,
    /// Explicit absolute Unix socket path; independent from Runtime Home.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Positive Runtime event buffer capacity; defaults to 256.
    #[arg(long, value_name = "COUNT")]
    event_capacity: Option<NonZeroUsize>,
}

#[cfg(feature = "demo-client")]
#[derive(Clone, Debug, Default, Eq, PartialEq, clap::Args)]
pub(crate) struct DemoArguments {
    /// Absolute Runtime Home override; defaults to ~/.ez-assistant.
    #[arg(long, value_name = "PATH")]
    runtime_home: Option<PathBuf>,
    /// Explicit absolute Unix socket path.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeConfig {
    pub(crate) runtime_home: PathBuf,
    pub(crate) config_path: PathBuf,
    pub(crate) socket_path: PathBuf,
    pub(crate) event_capacity: NonZeroUsize,
}

#[cfg(feature = "demo-client")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DemoConfig {
    pub(crate) socket_path: PathBuf,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum ConfigError {
    #[error("the user home directory is unavailable; use --runtime-home")]
    HomeDirectoryUnavailable,
    #[error("Runtime Home must be absolute")]
    RelativeRuntimeHome,
    #[error("runtime socket path must be absolute")]
    RelativeSocketPath,
    #[error("runtime socket path is too long; use --socket with a shorter absolute path")]
    SocketPathTooLong,
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
        let socket_path = resolve_socket(&runtime_home, arguments.socket)?;
        Ok(Self {
            config_path: runtime_home.join(CONFIG_FILE),
            runtime_home,
            socket_path,
            event_capacity: arguments.event_capacity.unwrap_or_else(|| {
                NonZeroUsize::new(DEFAULT_EVENT_CAPACITY).expect("static capacity is non-zero")
            }),
        })
    }
}

#[cfg(feature = "demo-client")]
impl DemoConfig {
    pub(crate) fn resolve(arguments: DemoArguments) -> Result<Self, ConfigError> {
        let runtime_home = resolve_runtime_home(arguments.runtime_home)?;
        Ok(Self {
            socket_path: resolve_socket(&runtime_home, arguments.socket)?,
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

/// 只在 bootstrap 边界把 `~` 解析为绝对路径；后续代码始终持有完整路径。
fn default_runtime_home(home: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    home.map(|path| path.join(DEFAULT_RUNTIME_HOME_DIRECTORY))
        .ok_or(ConfigError::HomeDirectoryUnavailable)
}

fn resolve_socket(
    runtime_home: &std::path::Path,
    socket: Option<PathBuf>,
) -> Result<PathBuf, ConfigError> {
    let path = socket.unwrap_or_else(|| runtime_home.join(RUN_DIRECTORY).join(SOCKET_FILE));
    if !path.is_absolute() {
        return Err(ConfigError::RelativeSocketPath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        if path.as_os_str().as_bytes().len() + 1 > MAX_SOCKET_PATH_BYTES_WITH_NUL {
            return Err(ConfigError::SocketPathTooLong);
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn expect_serve(action: CliAction) -> ServeArguments {
        match action {
            CliAction::Serve(arguments) => arguments,
            #[cfg(feature = "demo-client")]
            CliAction::Demo(_) => panic!("serve action"),
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
    fn serve_requires_absolute_paths_and_positive_capacity() {
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
    fn serve_derives_config_and_socket_from_runtime_home() {
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
        assert_eq!(
            config.socket_path,
            home.join(RUN_DIRECTORY).join(SOCKET_FILE)
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
    fn rejects_overlong_socket_path() {
        let home = PathBuf::from("/").join("x".repeat(MAX_SOCKET_PATH_BYTES_WITH_NUL));
        assert_eq!(
            resolve_socket(&home, None),
            Err(ConfigError::SocketPathTooLong)
        );
    }

    #[cfg(feature = "demo-client")]
    #[test]
    fn demo_and_serve_resolve_the_same_explicit_socket() {
        let home = std::env::temp_dir().join("runtime-host-config-test");
        let path = std::env::temp_dir().join("runtime-host-config-test.sock");
        let serve = parse_cli([
            OsString::from("serve"),
            OsString::from("--runtime-home"),
            home.clone().into_os_string(),
            OsString::from("--socket"),
            path.clone().into_os_string(),
        ])
        .expect("serve");
        let demo = parse_cli([
            OsString::from("demo"),
            OsString::from("--runtime-home"),
            home.into_os_string(),
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
