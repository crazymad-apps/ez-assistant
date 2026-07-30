//! Blocking stdin isolation and interactive command delivery.

use std::io::{self, BufRead, Write};

use tokio::sync::mpsc;

const INPUT_CAPACITY: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputCommand {
    Text(String),
    State,
    Compact,
    Reset,
    Cancel,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputAction {
    Start(String),
    ShowState,
    Compact,
    QueueCompaction,
    Reset,
    Cancel,
    Quit,
    CancelAndQuit,
    Reject(&'static str),
}

pub(crate) type InputResult = Result<InputCommand, io::Error>;

pub(crate) fn spawn_stdin() -> io::Result<mpsc::Receiver<InputResult>> {
    let (sender, receiver) = mpsc::channel(INPUT_CAPACITY);
    std::thread::Builder::new()
        .name("runtime-harness-stdin".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            loop {
                print!("> ");
                if let Err(error) = io::stdout().flush() {
                    let _ = sender.blocking_send(Err(error));
                    break;
                }
                match read_command(&mut reader) {
                    Ok(command) => {
                        let should_quit = command == InputCommand::Quit;
                        if sender.blocking_send(Ok(command)).is_err() || should_quit {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error));
                        break;
                    }
                }
            }
        })?;
    Ok(receiver)
}

pub(crate) fn action_for(command: InputCommand, has_active_run: bool) -> InputAction {
    match (command, has_active_run) {
        (InputCommand::Text(text), false) => InputAction::Start(text),
        (InputCommand::Text(_), true) => {
            InputAction::Reject("a run is active; use /cancel or wait for completion")
        }
        (InputCommand::State, _) => InputAction::ShowState,
        (InputCommand::Compact, false) => InputAction::Compact,
        (InputCommand::Compact, true) => InputAction::QueueCompaction,
        (InputCommand::Reset, false) => InputAction::Reset,
        (InputCommand::Reset, true) => {
            InputAction::Reject("cannot reset while a run is active; use /cancel first")
        }
        (InputCommand::Cancel, false) => InputAction::Reject("there is no active run to cancel"),
        (InputCommand::Cancel, true) => InputAction::Cancel,
        (InputCommand::Quit, false) => InputAction::Quit,
        (InputCommand::Quit, true) => InputAction::CancelAndQuit,
    }
}

fn read_command(reader: &mut impl BufRead) -> io::Result<InputCommand> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(InputCommand::Quit);
        }
        if let Some(command) = parse_line(&line) {
            return Ok(command);
        }
    }
}

fn parse_line(line: &str) -> Option<InputCommand> {
    let line = line.trim();
    match line {
        "" => None,
        "/state" => Some(InputCommand::State),
        "/compact" => Some(InputCommand::Compact),
        "/reset" => Some(InputCommand::Reset),
        "/cancel" => Some(InputCommand::Cancel),
        "/quit" => Some(InputCommand::Quit),
        text => Some(InputCommand::Text(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn parses_text_commands_and_ignores_blank_lines() {
        assert_eq!(
            parse_line(" hello \n"),
            Some(InputCommand::Text("hello".to_owned()))
        );
        assert_eq!(parse_line("/state"), Some(InputCommand::State));
        assert_eq!(parse_line("/compact"), Some(InputCommand::Compact));
        assert_eq!(parse_line("/reset"), Some(InputCommand::Reset));
        assert_eq!(parse_line("/cancel"), Some(InputCommand::Cancel));
        assert_eq!(parse_line("/quit"), Some(InputCommand::Quit));
        assert_eq!(parse_line(" \n"), None);
    }

    #[test]
    fn eof_is_equivalent_to_quit() {
        let mut reader = Cursor::new("\n");
        assert_eq!(
            read_command(&mut reader).expect("read command"),
            InputCommand::Quit
        );
    }

    #[test]
    fn active_run_policy_rejects_new_text_and_reset_but_allows_control() {
        assert!(matches!(
            action_for(InputCommand::Text("next".to_owned()), true),
            InputAction::Reject(_)
        ));
        assert!(matches!(
            action_for(InputCommand::Reset, true),
            InputAction::Reject(_)
        ));
        assert_eq!(action_for(InputCommand::Cancel, true), InputAction::Cancel);
        assert_eq!(
            action_for(InputCommand::Quit, true),
            InputAction::CancelAndQuit
        );
        assert_eq!(
            action_for(InputCommand::State, true),
            InputAction::ShowState
        );
        assert_eq!(
            action_for(InputCommand::Compact, true),
            InputAction::QueueCompaction
        );
    }

    #[test]
    fn idle_policy_starts_resets_and_exits_without_cancellation() {
        assert_eq!(
            action_for(InputCommand::Text("next".to_owned()), false),
            InputAction::Start("next".to_owned())
        );
        assert_eq!(action_for(InputCommand::Reset, false), InputAction::Reset);
        assert_eq!(
            action_for(InputCommand::Compact, false),
            InputAction::Compact
        );
        assert!(matches!(
            action_for(InputCommand::Cancel, false),
            InputAction::Reject(_)
        ));
        assert_eq!(action_for(InputCommand::Quit, false), InputAction::Quit);
    }
}
