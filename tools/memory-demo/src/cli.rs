//! Memory Demo 命令行参数解析；解析阶段不读取 `.env` 或 credential。

use std::{ffi::OsString, path::PathBuf};

use crate::DemoError;

pub(crate) const HELP: &str = "\
Memory Demo — ez-assistant v0.5 memory validation host

USAGE:
  memory-demo chat --data-dir <DIR> [--session <ID>]
  memory-demo --help

COMMANDS:
  chat    Start an interactive memory demo session

CHAT OPTIONS:
  --data-dir <DIR>   Required directory for pinned memory, recall records, and sessions
  --session <ID>     Restore an existing session instead of creating a new one

INTERACTIVE COMMANDS:
  /state   Show the frozen session prompt and latest pinned Store state
  /new     Create a new session from the latest pinned Store
  /cancel  Cancel the active Agent execution
  /quit    Cancel any active execution, wait for cleanup, and exit
";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help,
    Chat {
        data_dir: PathBuf,
        session: Option<String>,
    },
}

pub(crate) fn parse_env() -> Result<Command, DemoError> {
    parse(std::env::args_os().skip(1).collect())
}

fn parse(args: Vec<OsString>) -> Result<Command, DemoError> {
    let Some(command) = args.first() else {
        return Ok(Command::Help);
    };
    if command == "--help" || command == "-h" || command == "help" {
        return if args.len() == 1 {
            Ok(Command::Help)
        } else {
            Err(DemoError::Cli("help does not accept arguments".to_owned()))
        };
    }
    if command != "chat" {
        return Err(DemoError::Cli(format!(
            "unknown command `{}`; run with --help",
            command.to_string_lossy()
        )));
    }

    let mut data_dir = None;
    let mut session = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].to_str() {
            Some("--data-dir") => {
                if data_dir.is_some() {
                    return Err(DemoError::Cli("--data-dir was provided twice".to_owned()));
                }
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| DemoError::Cli("--data-dir requires a directory".to_owned()))?;
                if value.is_empty() {
                    return Err(DemoError::Cli("--data-dir must not be empty".to_owned()));
                }
                data_dir = Some(PathBuf::from(value));
            }
            Some("--session") => {
                if session.is_some() {
                    return Err(DemoError::Cli("--session was provided twice".to_owned()));
                }
                index += 1;
                let value = args
                    .get(index)
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        DemoError::Cli("--session requires a UTF-8 session id".to_owned())
                    })?;
                session = Some(value.to_owned());
            }
            Some(argument) => {
                return Err(DemoError::Cli(format!(
                    "unknown chat argument `{argument}`"
                )));
            }
            None => {
                return Err(DemoError::Cli(
                    "chat option names must be valid UTF-8".to_owned(),
                ));
            }
        }
        index += 1;
    }
    let data_dir = data_dir.ok_or_else(|| {
        DemoError::Cli("chat requires --data-dir <DIR>; run with --help".to_owned())
    })?;
    Ok(Command::Chat { data_dir, session })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_help_and_chat_without_reading_environment() {
        assert_eq!(parse(vec![]).expect("default help"), Command::Help);
        assert_eq!(
            parse(args(&[
                "chat",
                "--data-dir",
                "/tmp/memory-demo",
                "--session",
                "session_1"
            ]))
            .expect("parse chat"),
            Command::Chat {
                data_dir: PathBuf::from("/tmp/memory-demo"),
                session: Some("session_1".to_owned()),
            }
        );
    }

    #[test]
    fn rejects_missing_data_dir_duplicate_and_unknown_arguments() {
        for invalid in [
            args(&["chat"]),
            args(&["chat", "--data-dir"]),
            args(&["chat", "--data-dir", "one", "--data-dir", "two"]),
            args(&["chat", "--data-dir", "one", "--unknown"]),
            args(&["unknown"]),
        ] {
            assert!(parse(invalid).is_err());
        }
    }
}
