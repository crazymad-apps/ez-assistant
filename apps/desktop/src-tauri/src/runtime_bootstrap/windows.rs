//! Desktop 自己的 discovery/实例锁读取边界，不依赖 Host 实现或业务 Runtime。
//! 私有文件沿用 owner + Profile ACL + 拒绝 reparse；原生停止临时核对文件身份，不修改 DACL。

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

pub(super) struct LockProcess {
    pid: u32,
    start: u64,
    image: PathBuf,
    handle: OwnedHandle,
}

impl LockProcess {
    /// MSI 已核实安装注册；按映像路径结束同一内核进程，不要求 Home、旧摘要或业务 HTTP。
    #[allow(unsafe_code)]
    pub(super) fn terminate_installation(&self, image: &Path) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        if self.exited()? {
            return Ok(());
        }
        if process_start(&self.handle)? != self.start
            || crate::runtime_source::canonical_home(&process_image(&self.handle)?).as_deref()
                != crate::runtime_source::canonical_home(&self.image).as_deref()
            || !same_installation_directory(image, &self.image)
            || !process_owned_by_current_user(&self.handle)?
        {
            return Err(io::Error::other("installation process identity changed"));
        }
        // SAFETY: 持有查询时的同一内核句柄；归属、创建时间和映像重新核实，无 PID 重开。
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 && !self.exited()? {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn started_at(&self) -> io::Result<std::time::SystemTime> {
        // FILETIME 为自 1601 年起的 100ns；转换只使用值，不再次按 PID 查询。
        let ticks = self
            .start
            .checked_sub(116_444_736_000_000_000)
            .ok_or_else(invalid)?;
        std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(ticks / 10_000_000))
            .and_then(|time| {
                time.checked_add(std::time::Duration::from_nanos((ticks % 10_000_000) * 100))
            })
            .ok_or_else(invalid)
    }
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }
    pub(super) fn image(&self) -> &Path {
        &self.image
    }

    #[allow(unsafe_code)]
    pub(super) fn terminate(&self, home: &Path, lock: &File) -> io::Result<()> {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        if self.exited()? {
            return Ok(());
        }
        let fresh = open_private_file(home, &home.join("run/runtime.lock"), true)?;
        let owns_lock = resource_processes(&home.join("run/runtime.lock"))?
            .iter()
            .any(|process| {
                process.dwProcessId == self.pid && filetime(process.ProcessStartTime) == self.start
            });
        if !same_lock(lock, &fresh)?
            || !owns_lock
            || process_start(&self.handle)? != self.start
            || process_image(&self.handle)? != self.image
            || !process_owned_by_current_user(&self.handle)?
        {
            return Err(io::Error::other("Runtime process or lock identity changed"));
        }
        // SAFETY: 从查询到结束一直持有同一进程内核句柄；最小 TERMINATE 权限，无 PID 重开。
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 {
            let error = io::Error::last_os_error();
            if !self.exited()? {
                return Err(error);
            }
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    pub(super) fn exited(&self) -> io::Result<bool> {
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        // SAFETY: 独占 OwnedHandle 在查询期间有效；零超时不阻塞，不用退出码猜测存活。
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

#[allow(unsafe_code)]
pub(super) fn lock_processes(home: &Path, _lock: &File) -> io::Result<Vec<LockProcess>> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    let mut candidates = Vec::new();
    for process in resource_processes(&home.join("run/runtime.lock"))? {
        if process.dwProcessId == 0 || process.dwProcessId == std::process::id() {
            continue;
        }
        // SAFETY: 仅查询、结束、等待目标，句柄不可继承，成功即交给 OwnedHandle 唯一回收。
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                process.dwProcessId,
            )
        };
        if raw.is_null() {
            continue;
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let start = process_start(&handle)?;
        if start != filetime(process.ProcessStartTime) || !process_owned_by_current_user(&handle)? {
            continue;
        }
        let candidate = LockProcess {
            pid: process.dwProcessId,
            start,
            image: process_image(&handle)?,
            handle,
        };
        if !candidate.exited()? {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

/// 安装模式同时查看进程快照和文件占用；文件已替换时不能只查询当前文件的持有者。
/// 仅匹配固定产品映像名后查询完整路径，其他安装不进入返回值；权限/归属未知则拒绝。
#[allow(unsafe_code)]
pub(super) fn installation_processes(image: &Path) -> io::Result<Vec<LockProcess>> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::{
        Foundation::{ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE,
            },
        },
    };
    let name = image.file_name().ok_or_else(invalid)?.to_string_lossy();
    // SAFETY: 只创建系统进程快照，无远端地址；成功句柄立即交给 OwnedHandle 唯一回收。
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut result = Vec::new();
    // SAFETY: 正确 dwSize 的独占 SDK 输出，快照在整个枚举期间存活。
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        let length = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(invalid)?;
        let candidate_name = std::ffi::OsString::from_wide(&entry.szExeFile[..length]);
        if entry.th32ProcessID != std::process::id()
            && candidate_name.to_string_lossy().eq_ignore_ascii_case(&name)
        {
            // SAFETY: 只申请本阶段的查询/结束/等待权限，成功后保持同一内核句柄至结束。
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                    0,
                    entry.th32ProcessID,
                )
            };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32) {
                    return Err(error);
                }
            } else {
                let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
                let path = process_image(&handle)?;
                // 快照保留启动时的映像名，QueryFullProcessImageName 会跟随文件重命名。
                // 已由快照核对固定产品名，此处核对同一注册安装目录，容许目录内旧文件换名。
                if same_installation_directory(image, &path) {
                    if !process_owned_by_current_user(&handle)? {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "another user owns the installation process",
                        ));
                    }
                    let process = LockProcess {
                        pid: entry.th32ProcessID,
                        start: process_start(&handle)?,
                        image: path,
                        handle,
                    };
                    if !process.exited()? {
                        result.push(process);
                    }
                }
            }
        }
        // SAFETY: 同一有效快照及输出缓冲区；停止后检查真实终止原因。
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        return Err(error);
    }
    Ok(result)
}

fn same_installation_directory(expected: &Path, actual: &Path) -> bool {
    match (
        crate::runtime_source::canonical_home(expected),
        crate::runtime_source::canonical_home(actual),
    ) {
        (Some(expected), Some(actual)) => expected.parent() == actual.parent(),
        _ => false,
    }
}

/// 只读最终占用复查；不批量结束其他打开文件的应用（例如防病毒或人工工具）。
pub(super) fn verify_installation_image(image: &Path) -> io::Result<()> {
    if !installation_processes(image)?.is_empty()
        || (image.exists() && !resource_processes(image)?.is_empty())
    {
        return Err(io::Error::other("installation image is still in use"));
    }
    Ok(())
}

struct ResourceSession(u32);

impl Drop for ResourceSession {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: 仅持有 RmStartSession 成功返回的 session；所有路径恰好结束一次，无批量关闭。
        unsafe {
            windows_sys::Win32::System::RestartManager::RmEndSession(self.0);
        }
    }
}

#[allow(unsafe_code)]
fn resource_processes(
    path: &Path,
) -> io::Result<Vec<windows_sys::Win32::System::RestartManager::RM_UNIQUE_PROCESS>> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS},
        System::RestartManager::*,
    };
    let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) || path.len() > 32767 {
        return Err(invalid());
    }
    path.push(0);
    let mut session = 0;
    let mut key = [0_u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: 固定 SDK 容量的输出 key/session；成功后立即 RAII 接管。只注册已知文件。
    let status = unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let session = ResourceSession(session);
    let paths = [path.as_ptr()];
    let status = unsafe {
        RmRegisterResources(session.0, 1, paths.as_ptr(), 0, ptr::null(), 0, ptr::null())
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut capacity = 16_u32;
    for _ in 0..3 {
        if capacity > 1024 {
            return Err(invalid());
        }
        let mut entries = vec![RM_PROCESS_INFO::default(); capacity as usize];
        let mut needed = 0;
        let mut count = capacity;
        let mut reasons = 0;
        // SAFETY: 传入的条数与实际分配一致；仅成功且数量有效时读取，更多数据有界重试。
        let status = unsafe {
            RmGetList(
                session.0,
                &mut needed,
                &mut count,
                entries.as_mut_ptr(),
                &mut reasons,
            )
        };
        if status == ERROR_MORE_DATA {
            capacity = needed.max(capacity * 2);
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        if count > capacity {
            return Err(invalid());
        }
        return Ok(entries
            .into_iter()
            .take(count as usize)
            .map(|entry| entry.Process)
            .collect());
    }
    Err(io::Error::other("Runtime lock owners changed during query"))
}

#[allow(unsafe_code)]
fn process_start(handle: &OwnedHandle) -> io::Result<u64> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: 所有输出为独立 SDK FILETIME；借用句柄覆盖查询周期。
    if unsafe {
        GetProcessTimes(
            handle.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(filetime(created))
}

fn filetime(time: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[allow(unsafe_code)]
fn process_image(handle: &OwnedHandle) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;
    let mut bytes = vec![0_u16; 32768];
    let mut length = bytes.len() as u32;
    // SAFETY: length 为 wchar 容量；成功且有界后才读取实际路径，不依赖 NUL 扫描。
    if unsafe {
        QueryFullProcessImageNameW(handle.as_raw_handle(), 0, bytes.as_mut_ptr(), &mut length)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length == 0 || length as usize >= bytes.len() {
        return Err(invalid());
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &bytes[..length as usize],
    )))
}

#[allow(unsafe_code)]
fn process_owned_by_current_user(process: &OwnedHandle) -> io::Result<bool> {
    let mut current = ptr::null_mut();
    let mut target = ptr::null_mut();
    // SAFETY: 只申请 Token 查询；每个成功句柄马上交给 RAII，即使后续失败也回收。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut current) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let current = unsafe { OwnedHandle::from_raw_handle(current) };
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut target) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let target = unsafe { OwnedHandle::from_raw_handle(target) };
    let current = token_user(&current)?;
    let target = token_user(&target)?;
    // SAFETY: 成功的 TOKEN_USER 缓冲区保持原址且存活，内部 SID 不跨调用保存。
    unsafe {
        let a = &*current.as_ptr().cast::<TOKEN_USER>();
        let b = &*target.as_ptr().cast::<TOKEN_USER>();
        Ok(!a.User.Sid.is_null()
            && !b.User.Sid.is_null()
            && IsValidSid(a.User.Sid) != 0
            && IsValidSid(b.User.Sid) != 0
            && EqualSid(a.User.Sid, b.User.Sid) != 0)
    }
}

#[allow(unsafe_code)]
fn token_user(token: &OwnedHandle) -> io::Result<Vec<usize>> {
    let mut length = 0;
    // SAFETY: 首次只查询长度；usize 保证 TOKEN_USER 对齐，有界分配且读取期间不移动。
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
    let mut data = vec![0_usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            data.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(data)
}

#[allow(unsafe_code)]
pub(super) fn same_lock(a: &File, b: &File) -> io::Result<bool> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut left = BY_HANDLE_FILE_INFORMATION::default();
    let mut right = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: 只读取两个仍存活文件句柄的卷和文件索引，SDK 输出缓冲区完整。
    if unsafe { GetFileInformationByHandle(a.as_raw_handle(), &mut left) } == 0
        || unsafe { GetFileInformationByHandle(b.as_raw_handle(), &mut right) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(left.dwVolumeSerialNumber == right.dwVolumeSerialNumber
        && left.nFileIndexHigh == right.nFileIndexHigh
        && left.nFileIndexLow == right.nFileIndexLow)
}
