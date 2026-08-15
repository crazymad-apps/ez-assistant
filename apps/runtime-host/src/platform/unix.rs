//! Unix detached Runtime 启动适配。

use std::{
    io,
    path::Path,
    process::{Command, Stdio},
};

use std::os::unix::process::CommandExt as _;

/// 启动不继承 Desktop 标准流、拥有独立进程组的正式 `serve` 进程。
pub(crate) fn launch_detached(runtime_home: &Path) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable)
        .arg("serve")
        .arg("--runtime-home")
        .arg(runtime_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}
