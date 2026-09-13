//! 固定交互 Shell 的平台探测；不接收客户端提供的程序或参数。

use super::{TerminalError, failure};
use assistant_protocol::ShellKind;
use portable_pty::CommandBuilder;

pub(super) fn interactive(kind: Option<ShellKind>) -> Result<CommandBuilder, TerminalError> {
    #[cfg(unix)]
    {
        match kind {
            None => Ok(CommandBuilder::new_default_prog()),
            Some(ShellKind::PosixSh) => {
                let mut command = CommandBuilder::new("/bin/sh");
                command.arg("-i");
                Ok(command)
            }
            _ => Err(failure("该 Shell 不适用于当前 Host 平台。")),
        }
    }
    #[cfg(windows)]
    {
        let kind = kind.unwrap_or(ShellKind::WindowsPowershell51);
        let program = crate::resources::shell::executable(kind)
            .ok_or_else(|| failure("所选 Shell 未安装或不适用于当前 Host，未自动切换。"))?;
        let mut command = CommandBuilder::new(program);
        match kind {
            ShellKind::WindowsPowershell51 | ShellKind::Powershell7 => {
                command.args(["-NoLogo", "-NoProfile"])
            }
            ShellKind::Cmd => command.arg("/d"),
            ShellKind::GitBash => command.arg("-i"),
            ShellKind::PosixSh => return Err(failure("该 Shell 不适用于当前 Host 平台。")),
        }
        Ok(command)
    }
}
