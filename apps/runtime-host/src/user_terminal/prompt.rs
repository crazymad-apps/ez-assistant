//! 进程局部提示符适配；不改用户文件、不从 PTY 输出猜测提示符，也不参与 Shell 目录探测。

#[cfg(test)]
mod tests;

use super::{TerminalError, failure};
use assistant_protocol::ShellKind as Shell;
use portable_pty::CommandBuilder;
use tempfile::TempDir;

/// 返回的启动目录必须由 PTY owner 持有到进程及 I/O 回收完成。
pub(super) fn configure(
    command: &mut CommandBuilder,
    shell: Option<Shell>,
) -> Result<Option<TempDir>, TerminalError> {
    match shell {
        Some(Shell::Bash | Shell::GitBash) => {
            if command.get_env("PS1").is_none() {
                command.env("PS1", r"\W \$ ");
            }
            let hook = include_str!("prompt/bash.sh");
            let previous = command.get_env("PROMPT_COMMAND").unwrap_or_default();
            // 原有环境钩子本来就是 Shell 代码；不把目录或其他用户数据拼入代码。
            let mut combined = previous.to_os_string();
            combined.push("\n");
            combined.push(hook);
            command.env("PROMPT_COMMAND", combined);
        }
        Some(Shell::Zsh) => return zsh(command).map(Some),
        Some(Shell::PosixSh) => {
            command.env("PS1", "$(if [ \"$PWD\" = / ]; then printf /; elif [ \"$PWD\" = \"$HOME\" ]; then printf '~'; else printf '%s' \"${PWD##*/}\"; fi | LC_ALL=C tr '[:cntrl:]' '?') $ ");
        }
        Some(Shell::WindowsPowershell51 | Shell::Powershell7) => {
            use base64::{Engine, engine::general_purpose::STANDARD};
            let bytes: Vec<_> = include_str!("prompt/powershell.ps1")
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            command.args(["-NoExit", "-EncodedCommand", &STANDARD.encode(bytes)]);
        }
        Some(Shell::Cmd) => command.env("PROMPT", "$N:$G$S"),
        None => {} // 未知默认程序保留自身启动与提示符语义。
    }
    Ok(None)
}

fn zsh(command: &mut CommandBuilder) -> Result<TempDir, TerminalError> {
    let directory = tempfile::Builder::new()
        .prefix("ez-terminal-zsh-")
        .tempdir()
        .map_err(|_| failure("无法准备终端启动文件。"))?;
    for (name, body) in [
        (".zshenv", include_str!("prompt/zshenv")),
        (".zprofile", include_str!("prompt/zprofile")),
        (".zshrc", include_str!("prompt/zshrc")),
        ("prompt.zsh", include_str!("prompt/zsh.zsh")),
    ] {
        std::fs::write(directory.path().join(name), body)
            .map_err(|_| failure("无法写入终端启动文件。"))?;
    }
    if let Some(original) = command.get_env("ZDOTDIR").map(ToOwned::to_owned) {
        command.env("EZ_TERMINAL_USER_ZDOTDIR", original);
        command.env("EZ_TERMINAL_HAS_ZDOTDIR", "1");
    } else {
        command.env_remove("EZ_TERMINAL_USER_ZDOTDIR");
        command.env_remove("EZ_TERMINAL_HAS_ZDOTDIR");
    }
    command.env("ZDOTDIR", directory.path());
    if command.get_env("PS1").is_none() {
        command.env("PS1", "%~ %# ");
    }
    Ok(directory)
}
