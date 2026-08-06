//! Core Demo 私有命令行解析。

use std::{ffi::OsString, path::PathBuf};

use thiserror::Error;

pub(crate) const HELP: &str = "\
Core Demo - Agent Core candidate SDK validation host

USAGE:
    core-demo --help
    core-demo serve --workdir <PATH> --data-dir <PATH> [--port <0..65535>]
        [--max-compaction-handoffs <N>] [--retry-transient]

The serve command uses DeepSeek through the OpenAI-compatible adapter. Configure
DEEPSEEK_API_KEY and optionally DEEPSEEK_BASE_URL, DEEPSEEK_MODEL, and
DEEPSEEK_CONTEXT_WINDOW_TOKENS in the process environment or repository .env.
Transient establishment retry is disabled unless --retry-transient is present.
Context compaction handoffs default to 2 per Run.
The server always listens on 127.0.0.1; port defaults to 0 (assigned by the OS).
Pinned memory and recall records use the data directory; Session journals remain in memory.
The data directory is not isolated from file or Shell tools; use a dedicated disposable directory.
";

pub(crate) const DEFAULT_MAX_COMPACTION_HANDOFFS: u32 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeArguments {
    pub workdir: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
    pub max_compaction_handoffs: u32,
    pub retry_transient: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Help,
    Serve(ServeArguments),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum CliError {
    #[error("missing command; use --help for usage")]
    MissingCommand,
    #[error("unknown argument `{0}`; use --help for usage")]
    UnknownArgument(String),
    #[error("option `{0}` requires a value")]
    MissingValue(&'static str),
    #[error("required option `{0}` was not provided")]
    MissingOption(&'static str),
    #[error("port must be an integer from 0 to 65535")]
    InvalidPort,
    #[error("max compaction handoffs must be a non-negative integer")]
    InvalidMaxCompactionHandoffs,
}

pub(crate) fn parse_cli(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<CliAction, CliError> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(CliError::MissingCommand);
    };
    if command == "--help" || command == "-h" {
        if let Some(extra) = arguments.next() {
            return Err(CliError::UnknownArgument(
                extra.to_string_lossy().into_owned(),
            ));
        }
        return Ok(CliAction::Help);
    }
    if command != "serve" {
        return Err(CliError::UnknownArgument(
            command.to_string_lossy().into_owned(),
        ));
    }

    let mut workdir = None;
    let mut data_dir = None;
    let mut port = 0;
    let mut max_compaction_handoffs = DEFAULT_MAX_COMPACTION_HANDOFFS;
    let mut retry_transient = false;
    while let Some(argument) = arguments.next() {
        match argument.to_string_lossy().as_ref() {
            "--workdir" => {
                workdir = Some(PathBuf::from(next_value(&mut arguments, "--workdir")?));
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(next_value(&mut arguments, "--data-dir")?));
            }
            "--port" => {
                port = next_value(&mut arguments, "--port")?
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| CliError::InvalidPort)?;
            }
            "--max-compaction-handoffs" => {
                max_compaction_handoffs = next_value(&mut arguments, "--max-compaction-handoffs")?
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| CliError::InvalidMaxCompactionHandoffs)?;
            }
            "--retry-transient" => retry_transient = true,
            unknown => return Err(CliError::UnknownArgument(unknown.to_owned())),
        }
    }

    Ok(CliAction::Serve(ServeArguments {
        workdir: workdir.ok_or(CliError::MissingOption("--workdir"))?,
        data_dir: data_dir.ok_or(CliError::MissingOption("--data-dir"))?,
        port,
        max_compaction_handoffs,
        retry_transient,
    }))
}

fn next_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &'static str,
) -> Result<OsString, CliError> {
    let value = arguments.next().ok_or(CliError::MissingValue(option))?;
    if value.to_string_lossy().starts_with("--") {
        return Err(CliError::MissingValue(option));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serve_arguments() {
        let action = parse_cli(
            [
                "serve",
                "--workdir",
                "project",
                "--data-dir",
                "state",
                "--port",
                "7070",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .expect("parse serve arguments");

        assert_eq!(
            action,
            CliAction::Serve(ServeArguments {
                workdir: PathBuf::from("project"),
                data_dir: PathBuf::from("state"),
                port: 7070,
                max_compaction_handoffs: DEFAULT_MAX_COMPACTION_HANDOFFS,
                retry_transient: false,
            })
        );
    }

    #[test]
    fn parses_compaction_and_retry_options() {
        let action = parse_cli(
            [
                "serve",
                "--workdir",
                "project",
                "--data-dir",
                "state",
                "--max-compaction-handoffs",
                "4",
                "--retry-transient",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .expect("parse serve arguments");

        let CliAction::Serve(arguments) = action else {
            panic!("expected serve action");
        };
        assert_eq!(arguments.max_compaction_handoffs, 4);
        assert!(arguments.retry_transient);
    }

    #[test]
    fn rejects_missing_required_options() {
        assert_eq!(
            parse_cli([OsString::from("serve")]),
            Err(CliError::MissingOption("--workdir"))
        );
    }

    #[test]
    fn rejects_another_option_where_a_value_is_required() {
        assert_eq!(
            parse_cli(
                ["serve", "--workdir", "--data-dir", "state"]
                    .into_iter()
                    .map(OsString::from)
            ),
            Err(CliError::MissingValue("--workdir"))
        );
    }
}
