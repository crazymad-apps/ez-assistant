//! Safety Demo 进程参数。

use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use agent_tools::AbsolutePath;
use thiserror::Error;

pub(crate) const HELP: &str = "\
Safety Demo\n\
\n\
Usage:\n\
  safety-demo --workdir <path> [--port <port>]\n\
\n\
Options:\n\
  --workdir <path>  Session working directory (required)\n\
  --port <port>     Loopback port; defaults to 0 (OS assigned)\n\
  -h, --help        Show this help\n";

/// 通过校验的进程配置。监听地址不对外暴露为配置，固定为 loopback。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessConfig {
    session_workdir: AbsolutePath,
    port: u16,
}

impl ProcessConfig {
    pub(crate) fn session_workdir(&self) -> &AbsolutePath {
        &self.session_workdir
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Run(ProcessConfig),
    Help,
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("--workdir is required")]
    MissingWorkdir,
    #[error("{option} requires a value")]
    MissingValue { option: &'static str },
    #[error("{option} may only be provided once")]
    DuplicateOption { option: &'static str },
    #[error("unknown argument: {argument}")]
    UnknownArgument { argument: String },
    #[error("--port must be an integer from 0 to 65535")]
    InvalidPort,
    #[error("current directory is invalid: {message}")]
    InvalidCurrentDirectory { message: String },
    #[error("workdir is invalid: {message}")]
    InvalidWorkdir { message: String },
    #[error("workdir is not a directory: {path}")]
    WorkdirNotDirectory { path: PathBuf },
}

pub(crate) fn parse_env() -> Result<CliAction, ConfigError> {
    let current_dir =
        std::env::current_dir().map_err(|error| ConfigError::InvalidCurrentDirectory {
            message: error.to_string(),
        })?;
    parse_from(std::env::args_os().skip(1), current_dir)
}

fn parse_from(
    arguments: impl IntoIterator<Item = OsString>,
    current_dir: PathBuf,
) -> Result<CliAction, ConfigError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == OsStr::new("--help") || argument == OsStr::new("-h"))
    {
        return Ok(CliAction::Help);
    }

    let current_dir =
        AbsolutePath::new(current_dir).map_err(|error| ConfigError::InvalidCurrentDirectory {
            message: error.to_string(),
        })?;
    let mut workdir = None;
    let mut port = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.to_str() {
            Some("--workdir") => {
                set_once(&mut workdir, "--workdir")?;
                index += 1;
                let value = arguments.get(index).ok_or(ConfigError::MissingValue {
                    option: "--workdir",
                })?;
                if value.to_string_lossy().starts_with('-') {
                    return Err(ConfigError::MissingValue {
                        option: "--workdir",
                    });
                }
                workdir = Some(value.clone());
            }
            Some("--port") => {
                set_once(&mut port, "--port")?;
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or(ConfigError::MissingValue { option: "--port" })?;
                if value.to_string_lossy().starts_with('-') {
                    return Err(ConfigError::MissingValue { option: "--port" });
                }
                let value = value.to_str().ok_or(ConfigError::InvalidPort)?;
                port = Some(value.parse::<u16>().map_err(|_| ConfigError::InvalidPort)?);
            }
            _ => {
                return Err(ConfigError::UnknownArgument {
                    argument: argument.to_string_lossy().into_owned(),
                });
            }
        }
        index += 1;
    }

    let workdir = workdir.ok_or(ConfigError::MissingWorkdir)?;
    let workdir = resolve_workdir(&current_dir, workdir)?;
    Ok(CliAction::Run(ProcessConfig {
        session_workdir: workdir,
        port: port.unwrap_or(0),
    }))
}

fn set_once<T>(slot: &mut Option<T>, option: &'static str) -> Result<(), ConfigError> {
    if slot.is_some() {
        Err(ConfigError::DuplicateOption { option })
    } else {
        Ok(())
    }
}

fn resolve_workdir(
    current_dir: &AbsolutePath,
    input: OsString,
) -> Result<AbsolutePath, ConfigError> {
    let path = PathBuf::from(input);
    let path = if path.is_absolute() {
        path
    } else {
        current_dir.as_path().join(path)
    };
    let path = AbsolutePath::new(path).map_err(|error| ConfigError::InvalidWorkdir {
        message: error.to_string(),
    })?;
    let metadata =
        std::fs::metadata(path.as_path()).map_err(|error| ConfigError::InvalidWorkdir {
            message: error.to_string(),
        })?;
    if !metadata.is_dir() {
        return Err(ConfigError::WorkdirNotDirectory {
            path: path.as_path().to_path_buf(),
        });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn config_requires_workdir_and_defaults_port_to_zero() {
        let root = tempfile::tempdir().expect("create temp root");
        let project = root.path().join("project");
        std::fs::create_dir(&project).expect("create project directory");

        let action = parse_from(args(&["--workdir", "project"]), root.path().to_path_buf())
            .expect("parse config");
        let CliAction::Run(config) = action else {
            panic!("expected run action");
        };
        assert_eq!(config.session_workdir().as_path(), project);
        assert_eq!(config.port(), 0);
        assert!(matches!(
            parse_from(Vec::new(), root.path().to_path_buf()),
            Err(ConfigError::MissingWorkdir)
        ));
    }

    #[test]
    fn config_parses_port_help_and_rejects_bad_arguments() {
        let root = tempfile::tempdir().expect("create temp root");
        let workdir = root.path().as_os_str().to_owned();
        let action = parse_from(
            vec![
                OsString::from("--workdir"),
                workdir,
                OsString::from("--port"),
                OsString::from("4321"),
            ],
            root.path().to_path_buf(),
        )
        .expect("parse explicit port");
        let CliAction::Run(config) = action else {
            panic!("expected run action");
        };
        assert_eq!(config.port(), 4321);
        assert_eq!(
            parse_from(args(&["--help"]), root.path().to_path_buf()).expect("parse help"),
            CliAction::Help
        );
        assert!(matches!(
            parse_from(args(&["--unknown"]), root.path().to_path_buf()),
            Err(ConfigError::UnknownArgument { .. })
        ));
        assert!(matches!(
            parse_from(
                args(&["--workdir", ".", "--port", "70000"]),
                root.path().to_path_buf()
            ),
            Err(ConfigError::InvalidPort)
        ));
        assert!(matches!(
            parse_from(
                args(&["--workdir", ".", "--workdir", "."]),
                root.path().to_path_buf()
            ),
            Err(ConfigError::DuplicateOption {
                option: "--workdir"
            })
        ));
        assert!(matches!(
            parse_from(
                args(&["--workdir", "--port", "4321"]),
                root.path().to_path_buf()
            ),
            Err(ConfigError::MissingValue {
                option: "--workdir"
            })
        ));
        assert!(matches!(
            parse_from(
                args(&["--port", "--workdir", "."]),
                root.path().to_path_buf()
            ),
            Err(ConfigError::MissingValue { option: "--port" })
        ));
    }

    #[test]
    fn config_rejects_files_and_missing_directories() {
        let root = tempfile::tempdir().expect("create temp root");
        let file = root.path().join("file.txt");
        std::fs::write(&file, "content").expect("write file");

        assert!(matches!(
            parse_from(
                vec![OsString::from("--workdir"), file.into_os_string()],
                root.path().to_path_buf()
            ),
            Err(ConfigError::WorkdirNotDirectory { .. })
        ));
        assert!(matches!(
            parse_from(args(&["--workdir", "missing"]), root.path().to_path_buf()),
            Err(ConfigError::InvalidWorkdir { .. })
        ));
    }
}
