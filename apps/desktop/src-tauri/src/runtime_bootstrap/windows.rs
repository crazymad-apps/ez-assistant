//! Desktop 自己的 discovery/实例锁读取边界，不依赖 Host 实现或业务 Runtime。
//! 私有文件沿用方案 3.1：owner + Profile ACL + 拒绝 reparse，不增加 File ID 或 DACL 机制。

use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

fn invalid() -> io::Error {
    io::Error::other("unsafe Runtime discovery path")
}

/// 不创建、不修复权限、不读取数据库。owner 来自当前进程 TokenUser，失败一律拒绝。
pub(super) fn open_private_file(home: &Path, path: &Path, write: bool) -> io::Result<File> {
    if !home.is_absolute() || !path.starts_with(home) {
        return Err(invalid());
    }
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(invalid()),
        },
        _ => return Err(invalid()),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(invalid());
    }
    let mut current = PathBuf::from(format!("{}:\\", char::from(drive)));
    let parts: Vec<_> = components.collect();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err(invalid());
        };
        if name
            .as_encoded_bytes()
            .iter()
            .any(|b| matches!(b, 0 | b':'))
        {
            return Err(invalid());
        }
        current.push(name);
        let final_file = index + 1 == parts.len();
        let file = OpenOptions::new()
            .read(true)
            .write(final_file && write)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&current)?;
        let metadata = file.metadata()?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (final_file && !metadata.is_file())
            || (!final_file && !metadata.is_dir())
        {
            return Err(invalid());
        }
        // 系统盘与 Profile 上级不属于当前用户；仅私有 Home 及其子级要求相同 owner。
        if index + 3 >= home.components().count() && !owned_by_current_user(&file)? {
            return Err(invalid());
        }
        if final_file {
            return Ok(file);
        }
    }
    Err(invalid())
}

#[allow(unsafe_code)]
fn owned_by_current_user(file: &File) -> io::Result<bool> {
    let mut owner = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: File 覆盖整个读取周期；仅查询 owner，未请求输出为 null。
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = (|| {
        let mut token = ptr::null_mut();
        // SAFETY: 进程伪句柄无需释放；新 token 仅 QUERY，立即交给 OwnedHandle 恰好关闭一次。
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        // SAFETY: 零长度仅查询容量；有界 usize 缓冲区满足 TOKEN_USER 的对齐要求。
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut length,
            );
        }
        if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 65536 {
            return Err(invalid());
        }
        let mut bytes = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                bytes.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 成功读取 TOKEN_USER；owner 借自仍存活 descriptor，user SID 借自 bytes。
        unsafe {
            let user = &*bytes.as_ptr().cast::<TOKEN_USER>();
            Ok(!owner.is_null()
                && !user.User.Sid.is_null()
                && IsValidSid(owner) != 0
                && IsValidSid(user.User.Sid) != 0
                && EqualSid(owner, user.User.Sid) != 0)
        }
    })();
    // SAFETY: GetSecurityInfo 成功返回的 LocalAlloc 缓冲区在所有 SID 借用结束后释放一次。
    unsafe {
        LocalFree(descriptor);
    }
    result
}

#[cfg(test)]
mod tests;
