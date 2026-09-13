//! Windows owner 核验。仅借用文件句柄；token 与安全描述符在本次调用内释放。

use std::{
    fs::OpenOptions,
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct Token(HANDLE);
impl Drop for Token {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: 只接收 OpenProcessToken 成功返回的独占有效句柄；Drop 恰好关闭一次。
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Descriptor(*mut std::ffi::c_void);
impl Drop for Descriptor {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo 返回的缓冲区由 LocalAlloc 分配；所有 SID 借用先于此 Drop 结束。
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// 校验当前路径的 owner；私有文件按设计不承诺同用户换文件防护，所有失败均向调用方返回。
#[allow(unsafe_code)]
pub(super) fn owned_by_current_user(path: &Path) -> io::Result<bool> {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut owner = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: file 在调用及比较期间保持打开；输出参数有效，未请求的字段传 null。
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
    let _descriptor = Descriptor(descriptor);
    let mut token = ptr::null_mut();
    // SAFETY: 当前进程伪句柄无需关闭；成功创建的 token 交给 RAII，仅申请 QUERY。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(token);
    let mut length = 0;
    // SAFETY: 首次传 null 与零长度仅查询所需容量，API 写入有效的 length 指针。
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut length);
    }
    if length < std::mem::size_of::<TOKEN_USER>() as u32 || length > 64 * 1024 {
        return Err(io::Error::other("invalid token user buffer size"));
    }
    // usize 存储保证 TOKEN_USER 所需指针对齐，容量上取整；缓冲区在 SID 比较期间不移动。
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: 分配至少 length 字节，token 有效；结构与内部 SID 由系统在同一缓冲区填入。
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 上一步成功返回 TOKEN_USER；owner 借自仍存活的 descriptor，user 借自 buffer。
    unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        Ok(!owner.is_null()
            && !user.User.Sid.is_null()
            && IsValidSid(owner) != 0
            && IsValidSid(user.User.Sid) != 0
            && EqualSid(owner, user.User.Sid) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_checks_real_file_directory_and_missing_path() {
        let root = tempfile::tempdir().unwrap();
        assert!(owned_by_current_user(root.path()).unwrap());
        let file = root.path().join("owner.txt");
        std::fs::write(&file, b"owner fixture").unwrap();
        assert!(owned_by_current_user(&file).unwrap());
        assert!(owned_by_current_user(&root.path().join("missing")).is_err());
        let system = std::env::var_os("SystemRoot").unwrap();
        // Windows 系统目录通常归 TrustedInstaller 或 Administrators，不能当作当前用户 Home。
        assert!(!owned_by_current_user(&Path::new(&system).join("System32")).unwrap());
    }
}
