//! 交互目录与启动共用固定程序解析；不接收客户端路径，不改变 Agent Shell 设置。

#[cfg(test)]
mod tests;

use super::{TerminalError, failure};
use assistant_protocol::{ShellKind as Shell, UserTerminalShellEntry};
use portable_pty::CommandBuilder;
use std::path::PathBuf;

pub(crate) fn catalog() -> Vec<UserTerminalShellEntry> {
    std::iter::once(None)
        .chain(
            crate::resources::shell::catalog()
                .into_iter()
                .filter(|entry| entry.available)
                .map(|entry| Some(entry.kind)),
        )
        .filter_map(|kind| {
            interactive(kind).ok().map(|(_, mut entry)| {
                entry.shell = kind;
                entry
            })
        })
        .collect()
}

/// 一次解析同时返回启动命令及显示事实；调用方不能再次探测后拼接回执。
pub(super) fn interactive(
    kind: Option<Shell>,
) -> Result<(CommandBuilder, UserTerminalShellEntry), TerminalError> {
    let program = match kind {
        #[cfg(unix)]
        None => default_program()?,
        #[cfg(windows)]
        None => crate::resources::shell::executable(Shell::WindowsPowershell51)
            .ok_or_else(unavailable)?,
        Some(kind) => crate::resources::shell::executable(kind).ok_or_else(unavailable)?,
    };
    prepare(kind, program)
}

fn prepare(
    kind: Option<Shell>,
    program: PathBuf,
) -> Result<(CommandBuilder, UserTerminalShellEntry), TerminalError> {
    if !crate::resources::shell::is_executable(&program) {
        return Err(unavailable());
    }
    let name = program
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
        .ok_or_else(unavailable)?
        .to_owned();
    #[cfg(unix)]
    let actual = kind.or(match name.as_str() {
        "sh" => Some(Shell::PosixSh),
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        _ => None,
    });
    #[cfg(windows)]
    let actual = Some(kind.unwrap_or(Shell::WindowsPowershell51));
    // 使用明确程序，防止 portable-pty 的 default_prog 在源失效时静默回退 sh。
    let mut command = CommandBuilder::new(&program);
    #[cfg(unix)]
    {
        command.env("SHELL", &program);
        // 默认保留登录启动语义；显式项保持普通交互启动，PTY 负责交互 I/O。
        command.arg(if kind.is_none() { "-l" } else { "-i" });
    }
    #[cfg(windows)]
    match actual {
        Some(Shell::WindowsPowershell51 | Shell::Powershell7) => {
            command.args(["-NoLogo", "-NoProfile"])
        }
        Some(Shell::Cmd) => command.arg("/d"),
        Some(Shell::GitBash) => command.arg("-i"),
        _ => return Err(unavailable()),
    }
    Ok((
        command,
        UserTerminalShellEntry {
            shell: actual,
            name,
        },
    ))
}

fn unavailable() -> TerminalError {
    failure("所选 Shell 未安装、不可执行或不适用于当前 Host，未自动切换。")
}

#[cfg(unix)]
fn default_program() -> Result<PathBuf, TerminalError> {
    // 与已有默认来源顺序一致：SHELL 优先，其次当前系统用户的登录 Shell；来源无效即报错。
    if let Some(program) = std::env::var_os("SHELL") {
        return Ok(program.into());
    }
    nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .map_err(|_| unavailable())?
        .map(|user| user.shell)
        .ok_or_else(unavailable)
}
