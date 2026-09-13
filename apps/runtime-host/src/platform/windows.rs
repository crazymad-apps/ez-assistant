//! Windows 平台原语（v0.25.3 技术方案 3.1）。
//!
//! 安全模型："owner 校验 + 用户 Profile ACL 继承 + 一律拒绝 reparse point"。
//! 保密性依赖 `%USERPROFILE%` 的用户级 ACL，软件层不重写 DACL、不逐文件复检 mode；
//! 一切 reparse point（symlink/junction/mount point）一律拒绝，防路径逃逸。
//! 目录 fsync 无 Windows 等价物，原子替换降级为 rename 后重读内容校验。

pub(crate) mod job;
pub(crate) mod resource_file;
mod security;

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 以 DETACHED_PROCESS 启动无控制台的正式 `serve` 进程，等价 Unix 的脱离会话。
pub(crate) fn launch_detached(runtime_home: &Path) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable)
        .args(["serve", "--detached", "--runtime-home"])
        .arg(runtime_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

/// Windows 无控制会话概念；从控制台启动时保持附加，便于日志可见。
pub(crate) fn detach_session() -> io::Result<()> {
    Ok(())
}

/// 私有性由 Profile ACL 继承承担；打开最终 reparse 本身，交由元数据检查拒绝。
pub(crate) fn apply_private_open_options(options: &mut OpenOptions) {
    apply_read_no_follow(options);
}

/// 不跟随最终 reparse；调用方仍须核验打开句柄的元数据。
pub(crate) fn apply_read_no_follow(options: &mut OpenOptions) {
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
}

/// 读取真实文件 owner 并与当前进程 TokenUser 比较；读取失败也拒绝，不使用用户名或环境变量推断。
pub(crate) fn owned_by_current_user(path: &Path, _metadata: &fs::Metadata) -> bool {
    security::owned_by_current_user(path).unwrap_or(false)
}

/// 私有文件按技术方案 3.1 不做强身份（File Index API 未稳定，且同用户攻击者可直接
/// 读取内容）。以"长度 + 修改时间 + 创建时间"做打开前后一致性核对：Windows 的
/// CopyFile 会保留 LastWriteTime，因此创建时间是检测换文件的关键信号；任一时间戳
/// 不可得时退化为仅比较可得的字段。
pub(crate) fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    if left.is_file() != right.is_file() || left.len() != right.len() {
        return false;
    }
    for (a, b) in [
        (left.created(), right.created()),
        (left.modified(), right.modified()),
    ] {
        match (a, b) {
            (Ok(a), Ok(b)) if a != b => return false,
            _ => {}
        }
    }
    true
}

/// 一切 reparse point（symlink/junction/mount point）一律拒绝，不做种类区分。
/// 检查原始属性位而非 `is_symlink()`，后者不覆盖全部 reparse tag。
pub(crate) fn is_reparse_or_link(metadata: &fs::Metadata) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// ACL 继承已保证私有性，无需软件层收紧。
pub(crate) fn tighten_private_file(_file: &File) -> io::Result<()> {
    Ok(())
}

/// 同 [`tighten_private_file`]。
pub(crate) fn tighten_private_file_at(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// 目录安全目标为"真实目录且非 reparse point"；调用方已完成该核验。
pub(crate) fn tighten_private_directory(_path: &Path, _initial: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

/// 私有文件安全等价条件：普通文件且不是 reparse point。
pub(crate) fn private_file_mode_is_secured(metadata: &fs::Metadata) -> bool {
    !is_reparse_or_link(metadata)
}

/// Windows 无逐文件 mode 语义；"精确私有"与"已私有"等价。
pub(crate) fn private_file_mode_is_exact(metadata: &fs::Metadata) -> bool {
    !is_reparse_or_link(metadata)
}

/// 私有目录安全等价条件：真实目录且不是 reparse point。
pub(crate) fn private_directory_mode_is_secured(metadata: &fs::Metadata) -> bool {
    metadata.is_dir() && !is_reparse_or_link(metadata)
}

/// 稳定文件防误删：设置只读属性。
pub(crate) fn set_private_readonly_at(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let mut permissions = metadata.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
}

/// Windows 目录句柄 fsync 不提供等价保证；持久化由 [`verify_replacement_contents`] 承担。
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Windows 的 FlushFileBuffers 需要写权限句柄，只读句柄 sync_all 会得到
/// ERROR_ACCESS_DENIED；因此以读写方式重开再落盘。
pub(crate) fn sync_file_at(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
}

/// rename 后重读目标文件并比对字节，替代 Unix 的目录 fsync；不一致视为替换失败。
pub(crate) fn verify_replacement_contents(path: &Path, expected: &[u8]) -> io::Result<()> {
    let actual = fs::read(path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::other(
            "replaced file contents did not match after rename",
        ))
    }
}
