//! Host 交互终端与 Agent 共用固定解释器探测。
#[cfg(test)]
mod tests;

use assistant_protocol::ShellKind;

#[cfg(unix)]
pub(crate) fn executable(kind: ShellKind) -> Option<std::path::PathBuf> {
    (kind == ShellKind::PosixSh).then(|| std::path::PathBuf::from("/bin/sh"))
}

pub(crate) fn catalog() -> Vec<assistant_protocol::ShellCatalogEntry> {
    [
        ShellKind::WindowsPowershell51,
        ShellKind::Cmd,
        ShellKind::Powershell7,
        ShellKind::GitBash,
        ShellKind::PosixSh,
    ]
    .into_iter()
    .map(|kind| {
        let available = executable(kind).is_some();
        assistant_protocol::ShellCatalogEntry {
            kind,
            available,
            reason: (!available).then(|| "未安装或不适用于当前 Host。".to_owned()),
        }
    })
    .collect()
}

pub(crate) fn freeze(
    kind: Option<ShellKind>,
) -> Result<assistant_runtime::FrozenShellEnvironment, assistant_runtime::RunToolFactoryError> {
    let kind = kind.unwrap_or(if cfg!(windows) {
        ShellKind::WindowsPowershell51
    } else {
        ShellKind::PosixSh
    });
    let program = executable(kind)
        .and_then(|path| path.into_os_string().into_string().ok())
        .ok_or_else(|| {
            assistant_runtime::RunToolFactoryError::new(
                assistant_runtime::RunToolFactoryErrorKind::InvalidConfiguration,
            )
        })?;
    let (args, prefix, dialect): (&[&str], &str, &str) = match kind {
        ShellKind::WindowsPowershell51 => (
            &["-NoProfile", "-NonInteractive", "-Command"],
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new(); $OutputEncoding=[Console]::OutputEncoding; ",
            "Windows PowerShell 5.1. Use PowerShell syntax: $env:NAME, single-quoted strings, backtick escaping and continuation. Use Windows drive paths; quote spaces. $? is success; $LASTEXITCODE is native exit status; use explicit exit to return a numeric status. Pipeline text encoding is UTF-8.",
        ),
        ShellKind::Powershell7 => (
            &["-NoProfile", "-NonInteractive", "-Command"],
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new(); $OutputEncoding=[Console]::OutputEncoding; ",
            "PowerShell 7. Use PowerShell syntax: $env:NAME, single-quoted strings, backtick escaping and continuation. Use Windows drive paths; quote spaces. $? is success; $LASTEXITCODE is native exit status; use explicit exit for a numeric status. Text encoding is UTF-8.",
        ),
        ShellKind::Cmd => (
            &["/d", "/s", "/c"],
            "chcp 65001>nul & ",
            "Windows CMD. Use %NAME% environment variables, double quotes for paths with spaces, caret escaping/continuation, and %ERRORLEVEL% or exit /b for status. Use Windows drive paths. AutoRun is disabled and the code page is UTF-8. PowerShell and POSIX syntax are not supported.",
        ),
        ShellKind::GitBash => (
            &["-c"],
            "",
            "Git for Windows Bash on Windows. Use Bash syntax: $NAME, single/double quotes, backslash escaping and continuation, $? status. Prefer /c/path drive paths. Commands run without a login profile; text encoding is UTF-8.",
        ),
        ShellKind::PosixSh => (
            &["-c"],
            "",
            "POSIX sh. Use $NAME, single/double quotes, backslash escaping and continuation, $? status, and POSIX paths. Do not assume Bash-specific syntax.",
        ),
    };
    Ok(assistant_runtime::FrozenShellEnvironment {
        kind,
        operating_system: std::env::consts::OS.to_owned(),
        program,
        fixed_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        command_prefix: prefix.to_owned(),
        dialect: dialect.to_owned(),
    })
}

pub(super) fn launcher(
    snapshot: &assistant_runtime::FrozenShellEnvironment,
) -> agent_tools_local::ShellLauncher {
    agent_tools_local::ShellLauncher {
        program: snapshot.program.clone().into(),
        fixed_args: snapshot.fixed_args.iter().map(Into::into).collect(),
        command_prefix: snapshot.command_prefix.clone(),
    }
}

#[cfg(windows)]
pub(crate) fn executable(kind: ShellKind) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    let path_program = |name: &str| {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|p| p.join(name))
                .find(|p| p.is_file())
        })
    };
    let program = match kind {
        ShellKind::WindowsPowershell51 => {
            windows.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        }
        ShellKind::Cmd => std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .unwrap_or_else(|| windows.join(r"System32\cmd.exe")),
        ShellKind::Powershell7 => path_program("pwsh.exe").or_else(|| {
            std::env::var_os("ProgramFiles")
                .map(|p| PathBuf::from(p).join(r"PowerShell\7\pwsh.exe"))
        })?,
        ShellKind::GitBash => {
            // 只接受 Git for Windows 的 bin 目录，不把 WSL/Cygwin 的同名 bash 当作 Git Bash。
            let valid = |path: &std::path::Path| {
                path.is_file() && path.parent().is_some_and(|p| p.join("git.exe").is_file())
            };
            let mut candidates = Vec::new();
            if let Some(path) = path_program("bash.exe") {
                candidates.push(path);
            }
            if let Some(path) = std::env::var_os("ProgramFiles") {
                candidates.push(PathBuf::from(path).join(r"Git\bin\bash.exe"));
            }
            if let Some(program) = candidates.iter().find(|p| valid(p)) {
                return Some(program.clone());
            }
            for hive in [
                r"HKEY_CURRENT_USER\SOFTWARE\GitForWindows",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\GitForWindows",
            ] {
                use std::os::windows::process::CommandExt;
                // 固定注册表键，经系统 PowerShell 的 Unicode API 读取并显式输出 UTF-8，
                // 不解析 reg.exe 的本地代码页输出，也不拼入客户端输入。
                let script = format!(
                    "[Console]::OutputEncoding=[Text.UTF8Encoding]::new(); (Get-ItemProperty -LiteralPath 'Registry::{hive}' -Name InstallPath -ErrorAction Stop).InstallPath"
                );
                let output = std::process::Command::new(
                    windows.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
                )
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    &script,
                ])
                .creation_flags(0x0800_0000)
                .output()
                .ok();
                if let Some(output) = output.filter(|o| o.status.success())
                    && let Ok(text) = String::from_utf8(output.stdout)
                    && !text.trim().is_empty()
                {
                    candidates.push(PathBuf::from(text.trim()).join(r"bin\bash.exe"));
                }
            }
            candidates.into_iter().find(|p| valid(p))?
        }
        ShellKind::PosixSh => return None,
    };
    program.is_file().then_some(program)
}
