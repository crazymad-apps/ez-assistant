//! Unix 平台原语：私有文件权限/属主/身份核验、目录同步与脱离启动。

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
};

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

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

/// 为新建或打开的私有文件附加 0600 权限与 `O_NOFOLLOW`。
pub(crate) fn apply_private_open_options(options: &mut OpenOptions) {
    options
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW);
}

/// 为只读打开附加 `O_NOFOLLOW`，关闭元数据检查与 open 之间的链接替换窗口。
pub(crate) fn apply_read_no_follow(options: &mut OpenOptions) {
    options.custom_flags(libc::O_NOFOLLOW);
}

/// 私有文件属主必须是当前进程用户。
pub(crate) fn owned_by_current_user(_path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.uid() == nix::unistd::geteuid().as_raw()
}

/// 同一文件的 dev/inode 身份比较，用于打开前后未被替换的核验。
pub(crate) fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

/// `symlink_metadata` 已不跟随链接；Unix 侧链接在此恒为真，供共享流程统一拒绝。
pub(crate) fn is_reparse_or_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// 把已打开句柄对应的文件权限收紧为 0600。
pub(crate) fn tighten_private_file(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

/// 把路径对应的文件权限收紧为 0600。
pub(crate) fn tighten_private_file_at(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

/// 通过目录句柄核验未被替换后把目录权限收紧为 0700，并复核最终 mode。
pub(crate) fn tighten_private_directory(path: &Path, initial: &fs::Metadata) -> io::Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    let opened = directory.metadata()?;
    if !opened.file_type().is_dir() || !same_file_identity(initial, &opened) {
        return Err(io::Error::other("directory was replaced during securing"));
    }
    directory.set_permissions(fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))?;
    let secured = directory.metadata()?;
    if secured.permissions().mode() & 0o777 != PRIVATE_DIRECTORY_MODE {
        return Err(io::Error::other("directory mode could not be secured"));
    }
    Ok(())
}

/// 私有文件权限是否满足"组/其他不可访问"。
pub(crate) fn private_file_mode_is_secured(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o077 == 0
}

/// 私有文件权限是否为精确的 0600；修复路径据此决定是否重新收紧。
pub(crate) fn private_file_mode_is_exact(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o777 == PRIVATE_FILE_MODE
}

/// 私有目录权限是否已收紧为 0700。
pub(crate) fn private_directory_mode_is_secured(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o777 == PRIVATE_DIRECTORY_MODE
}

/// 把稳定文件设为所有者只读（Unix 0400）。
pub(crate) fn set_private_readonly_at(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o400))
}

/// 目录 fsync，让 rename 等元数据变更落盘。
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// 对既有文件做一次落盘；POSIX 只读句柄即可 fsync。
pub(crate) fn sync_file_at(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Unix 已通过目录 fsync 保证替换落盘，重读校验为空实现。
pub(crate) fn verify_replacement_contents(_path: &Path, _expected: &[u8]) -> io::Result<()> {
    Ok(())
}
/// 布局备份保留链接目标，不跟随链接读取或移动外部数据。
pub(crate) fn copy_layout_link(
    target: &std::path::Path,
    link: &std::path::Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
