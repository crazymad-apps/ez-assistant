//! 本地文件系统与 Shell 基础设施 Adapter。
//!
//! 本 crate 持有真实本地 I/O 机制，不定义授权、审批、审计或应用工作模式策略。
//! Shell 在 Unix 上通过独立进程组清理后代，但无法约束主动调用 `setsid` / `setpgid`
//! 逃离进程组的进程；工作目录、逻辑路径和进程组都不是 OS 沙箱。

mod binary;
mod environment;
mod filesystem;
mod path_lock;
mod process;
mod search;
mod shell;

pub use binary::{BinaryReadError, read_binary_file};
pub use environment::EnvironmentPolicy;
pub use filesystem::{LocalFileSystem, LocalFileSystemConfig};
pub use shell::{LocalShell, LocalShellConfig, ShellLauncher};
