//! Unix detached Runtime 启动适配。

use std::{
    io,
    path::Path,
    process::{Command, Stdio},
};

/// 启动不继承 Desktop 标准流、由内部 child 建立独立 session的正式 `serve` 进程。
pub(crate) fn launch_detached(runtime_home: &Path) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable)
        .arg("serve")
        .arg("--detached")
        .arg("--runtime-home")
        .arg(runtime_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Called synchronously before constructing Tokio or initializing Host state.
pub(crate) fn detach_session() -> io::Result<()> {
    nix::unistd::setsid().map(|_| ()).map_err(io::Error::from)
}
