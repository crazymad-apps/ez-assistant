#![cfg(unix)]

use super::*;
use crate::user_terminal::{TerminalEvent, process::TerminalProcess};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;

fn zsh_available() -> bool {
    let available = Path::new("/bin/zsh").is_file();
    assert!(available || !cfg!(target_os = "macos"), "macOS zsh missing");
    if !available {
        eprintln!("zsh not installed; its prompt behavior is not tested");
    }
    available
}

fn fixture() -> TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("f02-prompt-")
        .tempdir_in(root.canonicalize().unwrap())
        .unwrap()
}

struct Terminal {
    process: Arc<TerminalProcess>,
    events: mpsc::UnboundedReceiver<TerminalEvent>,
    startup: Option<std::path::PathBuf>,
    pending: Vec<u8>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.process.cancel();
    }
}

impl Terminal {
    async fn start(home: &Path, cwd: &Path, shell: Shell, login: bool) -> Self {
        let program = match shell {
            Shell::Bash => "/bin/bash",
            Shell::Zsh => "/bin/zsh",
            Shell::PosixSh => "/bin/sh",
            _ => unreachable!(),
        };
        let mut command = CommandBuilder::new(program);
        command.arg(if login { "-il" } else { "-i" });
        command.env("HOME", home);
        command.env("SHELL", program);
        for key in ["PS1", "PROMPT_COMMAND", "ZDOTDIR", "ENV", "BASH_ENV"] {
            command.env_remove(key);
        }
        let startup = configure(&mut command, Some(shell)).unwrap();
        let path = startup.as_ref().map(|value| value.path().to_owned());
        let (send, events) = mpsc::unbounded_channel();
        let process = TerminalProcess::spawn_command(
            cwd.to_owned(),
            portable_pty::PtySize::default(),
            command,
            startup,
            move |event| {
                send.send(event)
                    .map_err(|_| failure("test receiver closed"))
            },
        )
        .await
        .unwrap();
        Self {
            process,
            events,
            startup: path,
            pending: Vec::new(),
        }
    }

    async fn until(&mut self, needle: &str) -> String {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(start) = self
                    .pending
                    .windows(needle.len())
                    .position(|value| value == needle.as_bytes())
                {
                    return String::from_utf8_lossy(
                        &self
                            .pending
                            .drain(..start + needle.len())
                            .collect::<Vec<_>>(),
                    )
                    .into_owned();
                }
                match self.events.recv().await.unwrap() {
                    TerminalEvent::Output { bytes: block } => {
                        self.pending.extend(block);
                        self.process.acknowledge();
                    }
                    _ => panic!(
                        "unexpected terminal event; output={}",
                        String::from_utf8_lossy(&self.pending)
                    ),
                }
            }
        })
        .await;
        match result {
            Ok(text) => text,
            Err(_) => {
                self.process.close().await.unwrap();
                panic!(
                    "prompt timeout: {needle}; output={}",
                    String::from_utf8_lossy(&self.pending)
                );
            }
        }
    }

    fn input(&self, value: &str) {
        self.process.write(value.as_bytes()).unwrap();
    }

    async fn close(self) {
        self.process.close().await.unwrap();
        if let Some(path) = &self.startup {
            assert!(!path.exists(), "startup files outlived PTY");
        }
    }
}

#[tokio::test]
async fn bash_preserves_profiles_branch_text_status_and_path_data() {
    for login in [false, true] {
        let home = fixture();
        let cwd = home
            .path()
            .join("parent")
            .join("中文 space$(touch OWNED)`touch HACK`%");
        std::fs::create_dir_all(&cwd).unwrap();
        let profile = "export FIXTURE_LOADED=yes\nPS1='theme \\w branch(main) \\$ '\n";
        let file = home
            .path()
            .join(if login { ".bash_profile" } else { ".bashrc" });
        std::fs::write(&file, profile).unwrap();
        let mut terminal = Terminal::start(home.path(), &cwd, Shell::Bash, login).await;
        let first = terminal.until("branch(main)").await;
        assert!(first.contains("中文 space"), "{first}");
        assert!(!first.contains("/parent/"), "{first}");
        assert!(!cwd.join("OWNED").exists());
        assert!(!cwd.join("HACK").exists());
        terminal.input("(exit 7)\n");
        terminal.until("branch(main)").await;
        terminal.input("printf 'STATUS=<%s> CONFIG=<%s>\\n' \"$?\" \"$FIXTURE_LOADED\"\n");
        terminal.until("STATUS=<7> CONFIG=<yes>").await;
        terminal.until("branch(main)").await;
        terminal.input("cd /\n");
        assert!(
            terminal
                .until("branch(main)")
                .await
                .contains("theme / branch(main)")
        );
        terminal.input("cd\n");
        assert!(
            terminal
                .until("branch(main)")
                .await
                .contains("theme ~ branch(main)")
        );
        terminal.close().await;
        assert_eq!(std::fs::read_to_string(file).unwrap(), profile);
    }
}

#[tokio::test]
async fn zsh_preserves_startup_order_redirected_zdotdir_hooks_and_logout() {
    if !zsh_available() {
        return;
    }
    for login in [false, true] {
        let home = fixture();
        let config = home.path().join("config");
        let cwd = home.path().join("parent").join("中文 space$(touch OWNED)%");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let files = [
            (
                home.path().join(".zshenv"),
                "export FIXTURE_ORDER=env\nZDOTDIR=$HOME/config\n",
            ),
            (config.join(".zprofile"), "FIXTURE_ORDER+=,profile\n"),
            (
                config.join(".zshrc"),
                "FIXTURE_ORDER+=,rc\nprecmd() { PROMPT='theme %~ branch(main) %? > '; }\nfixture_hook() { export HOOK_SEEN=yes; }\nprecmd_functions=(fixture_hook)\n",
            ),
            (config.join(".zlogin"), "FIXTURE_ORDER+=,login\n"),
            (
                config.join(".zlogout"),
                "print -r -- logout > $HOME/logout.txt\n",
            ),
        ];
        for (path, body) in &files {
            std::fs::write(path, body).unwrap();
        }
        let mut terminal = Terminal::start(home.path(), &cwd, Shell::Zsh, login).await;
        let first = terminal.until("branch(main)").await;
        assert!(!first.contains("/parent/"), "{first}");
        assert!(first.contains("中文 space"), "{first}");
        assert!(terminal.startup.as_ref().unwrap().exists());
        terminal.input("(exit 7)\n");
        terminal.until("branch(main) 7 >").await;
        terminal.input("printf 'STATUS=<%s> ORDER=<%s> HOOK=<%s> ZDOT=<%s>\\n' \"$?\" \"$FIXTURE_ORDER\" \"$HOOK_SEEN\" \"$ZDOTDIR\"\n");
        let expected = if login {
            "env,profile,rc,login"
        } else {
            "env,rc"
        };
        let output = terminal
            .until(&format!("ZDOT=<{}>", config.display()))
            .await;
        assert!(
            output.contains(&format!("STATUS=<7> ORDER=<{expected}> HOOK=<yes>")),
            "{output}"
        );
        assert!(!cwd.join("OWNED").exists());
        terminal.input("exit 0\n");
        if login {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match terminal.events.recv().await.unwrap() {
                        TerminalEvent::Output { .. } => terminal.process.acknowledge(),
                        TerminalEvent::Exited { code } => {
                            assert_eq!(code, 0);
                            break;
                        }
                        _ => panic!("terminal failed before normal exit"),
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(home.path().join("logout.txt"))
                    .unwrap()
                    .trim(),
                "logout"
            );
        }
        terminal.close().await;
        for (path, body) in files {
            assert_eq!(std::fs::read_to_string(path).unwrap(), body);
        }
    }
}

#[tokio::test]
async fn posix_sh_shows_home_root_and_directory_without_evaluating_its_name() {
    let home = fixture();
    let cwd = home.path().join("parent/中文$(touch OWNED)");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut terminal = Terminal::start(home.path(), &cwd, Shell::PosixSh, false).await;
    let first = terminal.until(" $ ").await;
    assert!(first.contains("中文$(touch OWNED) $ "), "{first}");
    assert!(!cwd.join("OWNED").exists());
    terminal.input("(exit 7)\n");
    terminal.until(" $ ").await;
    terminal.input("printf 'STATUS=<%s>\\n' \"$?\"\n");
    terminal.until("STATUS=<7>").await;
    terminal.until(" $ ").await;
    terminal.input("cd /\n");
    terminal.until("/ $ ").await;
    terminal.input("cd\n");
    terminal.until("~ $ ").await;
    terminal.close().await;
}

#[tokio::test]
async fn failed_spawn_releases_private_startup_files() {
    let home = fixture();
    let mut command = CommandBuilder::new("/nonexistent/ez-f02-shell");
    command.env("HOME", home.path());
    let startup = configure(&mut command, Some(Shell::Zsh)).unwrap().unwrap();
    let path = startup.path().to_owned();
    assert!(
        TerminalProcess::spawn_command(
            home.path().join("absent"),
            portable_pty::PtySize::default(),
            command,
            Some(startup),
            |_| Ok(())
        )
        .await
        .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn zsh_prompt_transform_is_idempotent_and_preserves_dates_colors_and_literals() {
    if !zsh_available() {
        return;
    }
    let home = fixture();
    let script = format!(
        "{}\n{}",
        include_str!("zsh.zsh"),
        r#"
PROMPT='%D{%Y-%m-%d} %F{blue}%~%f %%~ %1~ %2d %{literal %d%}'
__ez_terminal_prompt
first=$PROMPT
__ez_terminal_prompt
[[ $PROMPT == $first ]] || exit 9
print -r -- "$PROMPT"
"#
    );
    let output = std::process::Command::new("/bin/zsh")
        .args(["-f", "-c", &script])
        .env("HOME", home.path())
        .env("ZDOTDIR", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "%D{%Y-%m-%d} %F{blue}%32<...<%1~%<<%f %%~ %32<...<%1~%<< %32<...<%1/%<< %{literal %d%}"
    );
}

#[tokio::test]
async fn long_and_control_character_directory_names_remain_data() {
    for shell in [Shell::Bash, Shell::Zsh, Shell::PosixSh] {
        if shell == Shell::Zsh && !zsh_available() {
            continue;
        }
        let home = fixture();
        let leaf = format!("中文 {}\x1b[31m\n$(touch OWNED)", "abcdef".repeat(10));
        let cwd = home.path().join(&leaf);
        std::fs::create_dir_all(&cwd).unwrap();
        match shell {
            Shell::Bash => {
                std::fs::write(home.path().join(".bashrc"), "PS1='BEGIN \\w END> '\n").unwrap()
            }
            Shell::Zsh => {
                std::fs::write(home.path().join(".zshrc"), "PROMPT='BEGIN %1~ END> '\n").unwrap()
            }
            _ => {}
        }
        let mut terminal = Terminal::start(home.path(), &cwd, shell, false).await;
        let output = terminal
            .until(if shell == Shell::PosixSh {
                " $ "
            } else {
                " END> "
            })
            .await;
        terminal.close().await;
        assert!(
            !output.contains("\x1b[31m"),
            "filename injected ANSI: {output:?}"
        );
        assert!(!cwd.join("OWNED").exists());
        if shell != Shell::PosixSh {
            let label = output
                .split("BEGIN ")
                .nth(1)
                .unwrap()
                .split(" END>")
                .next()
                .unwrap();
            assert!(label.chars().count() <= 32, "{shell:?}: {label:?}");
        }
    }
}
