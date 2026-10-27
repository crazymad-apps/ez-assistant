//! macOS 只读进程／FD 查询；信号发送前重验身份。查询与 kill 之间仍存在平台竞争窗口。

use std::{
    fs::File,
    io,
    mem::{MaybeUninit, size_of},
    os::unix::{ffi::OsStringExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

const MAX_PROCESSES: usize = 65536;
const MAX_FDS: usize = 65536;
const PROC_PIDFDVNODEINFO: i32 = 1;
const PROX_FDTYPE_VNODE: u32 = 1;

// libc 已提供 vnode_info；此两结构严格对应 SDK sys/proc_info.h。
#[repr(C)]
struct ProcFileInfo {
    open_flags: u32,
    status: u32,
    offset: i64,
    kind: i32,
    guard_flags: u32,
}

#[repr(C)]
struct VnodeFdInfo {
    file: ProcFileInfo,
    vnode: libc::vnode_info,
}

pub(super) struct LockProcess {
    pid: i32,
    uid: u32,
    start: (u64, u64),
    image: PathBuf,
}

impl LockProcess {
    /// 安装器只管理固定安装映像；不以名称或调用者传入的 PID 作为结束凭据。
    pub(super) fn terminate_installation(&self) -> io::Result<()> {
        if self.exited()? {
            return Ok(());
        }
        let current = process_info(self.pid)?;
        if self.uid != nix::unistd::geteuid().as_raw()
            || self.uid != current.pbi_uid
            || self.start != (current.pbi_start_tvsec, current.pbi_start_tvusec)
            || process_image(self.pid)? != self.image
        {
            return Err(changed());
        }
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.pid),
            nix::sys::signal::SIGKILL,
        )
        .or_else(|error| {
            if error == nix::errno::Errno::ESRCH {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(io::Error::from)
    }

    /// 从该进程实际打开的锁确定 Home，支持显式 --runtime-home，不猜默认目录。
    pub(super) fn runtime_homes(&self) -> io::Result<Vec<PathBuf>> {
        let mut homes = Vec::new();
        let fds = match process_fds(self.pid) {
            Ok(fds) => fds,
            Err(_) if self.exited()? => return Ok(homes),
            Err(error) => return Err(error),
        };
        for fd in fds {
            if fd.proc_fdtype != PROX_FDTYPE_VNODE {
                continue;
            }
            let Some(path) = vnode_path(self.pid, fd.proc_fd)? else {
                continue;
            };
            if path.file_name().is_some_and(|name| name == "runtime.lock")
                && let Some(run) = path.parent()
                && run.file_name().is_some_and(|name| name == "run")
                && let Some(home) = run.parent()
                && !homes.iter().any(|existing| existing == home)
            {
                homes.push(home.to_owned());
            }
        }
        Ok(homes)
    }
    pub(super) fn started_at(&self) -> io::Result<std::time::SystemTime> {
        std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(self.start.0))
            .and_then(|time| time.checked_add(std::time::Duration::from_micros(self.start.1)))
            .ok_or_else(changed)
    }
    pub(super) fn pid(&self) -> u32 {
        self.pid as u32
    }
    pub(super) fn image(&self) -> &Path {
        &self.image
    }

    pub(super) fn terminate(&self, home: &Path, lock: &File) -> io::Result<()> {
        let current = match process_info(self.pid) {
            Ok(current) => current,
            Err(error) if gone(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        let fresh = super::native_stop::open_lock(home)?.ok_or_else(changed)?;
        let before = lock.metadata()?;
        let now = fresh.metadata()?;
        if self.start != (current.pbi_start_tvsec, current.pbi_start_tvusec)
            || self.uid != current.pbi_uid
            || process_image(self.pid)? != self.image
            || before.dev() != now.dev()
            || before.ino() != now.ino()
            || !has_lock_fd(self.pid, lock)?
        {
            return Err(changed());
        }
        // nix 安全封装。PID/启动时间/owner/FD 刚复核，但 macOS 没有原子进程句柄式 kill。
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.pid),
            nix::sys::signal::SIGKILL,
        )
        .or_else(|error| {
            if error == nix::errno::Errno::ESRCH {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(io::Error::from)
    }

    pub(super) fn exited(&self) -> io::Result<bool> {
        match process_info(self.pid) {
            Ok(info) => Ok(info.pbi_status == libc::SZOMB
                || (info.pbi_start_tvsec, info.pbi_start_tvusec) != self.start),
            Err(error) if gone(&error) => Ok(true),
            Err(error) => Err(error),
        }
    }
}

/// root 安装准备也使用实际目标用户作范围检查；出现其他用户占用时先拒绝，不先杀一部分。
pub(super) fn installation_processes(image: &Path, uid: u32) -> io::Result<Vec<LockProcess>> {
    let mut processes = Vec::new();
    for pid in process_ids()? {
        if pid <= 0 || pid as u32 == std::process::id() {
            continue;
        }
        let Ok(info) = process_info(pid) else {
            continue;
        };
        if info.pbi_status == libc::SZOMB {
            continue;
        }
        let Ok(actual) = process_image(pid) else {
            continue;
        };
        if actual != image {
            continue;
        }
        if info.pbi_uid != uid {
            return Err(io::Error::other("installation is in use by another user"));
        }
        processes.push(LockProcess {
            pid,
            uid,
            start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
            image: actual,
        });
    }
    Ok(processes)
}

#[allow(unsafe_code)]
fn process_fds(pid: i32) -> io::Result<Vec<libc::proc_fdinfo>> {
    // SAFETY: 零长度只查容量；有界对齐缓冲区在调用期间独占，完整返回后才读取。
    let needed =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut count = (needed as usize).div_ceil(size_of::<libc::proc_fdinfo>()) + 16;
    for _ in 0..3 {
        if count > MAX_FDS {
            return Err(changed());
        }
        let mut fds: Vec<_> = (0..count)
            .map(|_| libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            })
            .collect();
        let capacity = fds.len() * size_of::<libc::proc_fdinfo>();
        // SAFETY: 对齐数组覆盖整个调用，系统返回的有效字节数在截断前逐项核对。
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                capacity as i32,
            )
        };
        if written <= 0 || !(written as usize).is_multiple_of(size_of::<libc::proc_fdinfo>()) {
            return Err(changed());
        }
        if written as usize >= capacity {
            count *= 2;
            continue;
        }
        fds.truncate(written as usize / size_of::<libc::proc_fdinfo>());
        return Ok(fds);
    }
    Err(changed())
}

#[repr(C)]
struct VnodeFdPathInfo {
    file: ProcFileInfo,
    vnode: libc::vnode_info_path,
}

#[allow(unsafe_code)]
fn vnode_path(pid: i32, fd: i32) -> io::Result<Option<PathBuf>> {
    let mut info = MaybeUninit::<VnodeFdPathInfo>::zeroed();
    // SAFETY: SDK vnode_fdinfowithpath = proc_fileinfo + vnode_info_path；只在完整写入后读取 NUL 前字节。
    let written = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            2,
            info.as_mut_ptr().cast(),
            size_of::<VnodeFdPathInfo>() as i32,
        )
    };
    if written != size_of::<VnodeFdPathInfo>() as i32 {
        return Ok(None);
    }
    let info = unsafe { info.assume_init() };
    let bytes: Vec<u8> = info
        .vnode
        .vip_path
        .into_iter()
        .flatten()
        .map(|byte| byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(bytes))))
}

pub(super) fn lock_processes(_home: &Path, lock: &File) -> io::Result<Vec<LockProcess>> {
    let mut processes = Vec::new();
    for pid in process_ids()? {
        if pid <= 0 || pid as u32 == std::process::id() {
            continue;
        }
        let Ok(info) = process_info(pid) else {
            continue;
        };
        if info.pbi_uid != nix::unistd::geteuid().as_raw() || info.pbi_status == libc::SZOMB {
            continue;
        }
        if !has_lock_fd(pid, lock).unwrap_or(false) {
            continue;
        }
        let Ok(image) = process_image(pid) else {
            continue;
        };
        processes.push(LockProcess {
            pid,
            uid: info.pbi_uid,
            start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
            image,
        });
    }
    Ok(processes)
}

#[allow(unsafe_code)]
fn process_ids() -> io::Result<Vec<i32>> {
    // SAFETY: 空指针／零长度只读取容量；后续缓冲区为对齐的 i32，系统写入不超过其容量。
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut capacity = needed as usize + 128;
    for _ in 0..3 {
        if capacity > MAX_PROCESSES {
            return Err(changed());
        }
        let mut pids = vec![0_i32; capacity];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * size_of::<i32>()) as i32,
            )
        };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            return Ok(pids);
        }
        capacity *= 2;
    }
    Err(changed())
}

#[allow(unsafe_code)]
fn process_info(pid: i32) -> io::Result<libc::proc_bsdinfo> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: SDK flavor 对应 libc 同名结构；缓冲区对齐且完整，成功精确写入后才读取。
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if count != size_of::<libc::proc_bsdinfo>() as i32 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() })
}

#[allow(unsafe_code)]
fn process_image(pid: i32) -> io::Result<PathBuf> {
    let mut bytes = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: 有界字节缓冲区覆盖调用；不保存系统指针，不读取 NUL 后内容。
    let count = unsafe { libc::proc_pidpath(pid, bytes.as_mut_ptr().cast(), bytes.len() as u32) };
    if count <= 0 || count as usize > bytes.len() {
        return Err(io::Error::last_os_error());
    }
    let length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(changed)?;
    bytes.truncate(length);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[allow(unsafe_code)]
fn has_lock_fd(pid: i32, lock: &File) -> io::Result<bool> {
    let metadata = lock.metadata()?;
    for fd in process_fds(pid)? {
        if fd.proc_fdtype != PROX_FDTYPE_VNODE {
            continue;
        }
        let mut vnode = MaybeUninit::<VnodeFdInfo>::zeroed();
        // SAFETY: SDK vnode_fdinfo 为 proc_fileinfo + libc::vnode_info；仅精确成功才读取整数 stat。
        let count = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEINFO,
                vnode.as_mut_ptr().cast(),
                size_of::<VnodeFdInfo>() as i32,
            )
        };
        if count != size_of::<VnodeFdInfo>() as i32 {
            continue;
        }
        let vnode = unsafe { vnode.assume_init() };
        if vnode.vnode.vi_stat.vst_dev == metadata.dev() as u32
            && vnode.vnode.vi_stat.vst_ino == metadata.ino()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn gone(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT))
}

fn changed() -> io::Error {
    io::Error::other("Runtime process or lock identity changed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_layout_and_current_process_identity() {
        assert_eq!(size_of::<ProcFileInfo>(), 24);
        assert_eq!(size_of::<VnodeFdInfo>(), 176);
        assert_eq!(size_of::<VnodeFdPathInfo>(), 1200);
        let info = process_info(std::process::id() as i32).unwrap();
        assert_eq!(info.pbi_uid, nix::unistd::geteuid().as_raw());
        assert_eq!(
            process_image(std::process::id() as i32).unwrap(),
            std::env::current_exe().unwrap()
        );
        assert!(
            process_ids()
                .unwrap()
                .contains(&(std::process::id() as i32))
        );
    }
}
