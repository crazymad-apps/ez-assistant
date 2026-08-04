//! 阻塞 stdin 隔离和交互命令状态策略。

use std::io::{self, BufRead, Write};

use tokio::sync::mpsc;

const INPUT_CAPACITY: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputCommand {
    Text(String),
    State,
    New,
    Cancel,
    Quit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputAction {
    Start(String),
    ShowState,
    New,
    Cancel,
    Quit,
    CancelAndQuit,
    Reject(&'static str),
}

pub(crate) type InputResult = Result<InputCommand, io::Error>;

pub(crate) fn spawn_stdin() -> io::Result<mpsc::Receiver<InputResult>> {
    let (sender, receiver) = mpsc::channel(INPUT_CAPACITY);
    std::thread::Builder::new()
        .name("memory-demo-stdin".to_owned())
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
        (InputCommand::New, false) => InputAction::New,
        (InputCommand::New, true) => {
            InputAction::Reject("cannot create a new session while a run is active")
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
        "/new" => Some(InputCommand::New),
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
    fn parses_commands_and_treats_eof_as_quit() {
        assert_eq!(
            parse_line("hello"),
            Some(InputCommand::Text("hello".to_owned()))
        );
        assert_eq!(parse_line("/state"), Some(InputCommand::State));
        assert_eq!(parse_line("/new"), Some(InputCommand::New));
        assert_eq!(parse_line("/cancel"), Some(InputCommand::Cancel));
        assert_eq!(parse_line("/quit"), Some(InputCommand::Quit));
        assert_eq!(parse_line("  "), None);
        assert_eq!(
            read_command(&mut Cursor::new("\n")).expect("read eof"),
            InputCommand::Quit
        );
    }

    #[test]
    fn active_policy_rejects_text_and_new_but_cancels_before_quit() {
        assert!(matches!(
            action_for(InputCommand::Text("next".to_owned()), true),
            InputAction::Reject(_)
        ));
        assert!(matches!(
            action_for(InputCommand::New, true),
            InputAction::Reject(_)
        ));
        assert_eq!(action_for(InputCommand::Cancel, true), InputAction::Cancel);
        assert_eq!(
            action_for(InputCommand::Quit, true),
            InputAction::CancelAndQuit
        );
    }
}
