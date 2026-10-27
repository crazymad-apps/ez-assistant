//! 本机停止只依赖实际锁占用与原生身份；不调用 Host HTTP，也不读取业务数据。

use super::{RuntimeBootstrapError, RuntimeBootstrapErrorCode, RuntimeDiscovery};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::Instant,
};

#[cfg(target_os = "macos")]
use super::macos as platform;
#[cfg(windows)]
use super::windows as platform;

pub(super) fn stop(
    home: &Path,
    executable: &Path,
    expected: Option<&RuntimeDiscovery>,
) -> Result<Option<String>, RuntimeBootstrapError> {
    if !home.is_absolute() {
        return Err(unverified());
    }
    let deadline = Instant::now() + super::SHUTDOWN_TIMEOUT;
    let snapshot = optional_discovery_with_modified(home)?;
    let modified = snapshot.as_ref().map(|(_, modified)| *modified);
    let original = snapshot.map(|(discovery, _)| discovery);
    if let Some(expected) = expected {
        ensure_same_discovery(Some(expected), original.as_ref())?;
    }
    let Some(lock) = open_lock(home).map_err(failed)? else {
        if original
            .as_ref()
            .or(expected)
            .is_some_and(|discovery| super::process_is_alive(discovery.pid))
        {
            return Err(unverified());
        }
        return Ok(original.map(|discovery| discovery.instance_id));
    };
    if available(&lock)? {
        if original
            .as_ref()
            .or(expected)
            .is_some_and(|discovery| super::process_is_alive(discovery.pid))
        {
            return Err(unverified());
        }
        return Ok(original.map(|discovery| discovery.instance_id));
    }
    if expected.is_some() && original.is_none() {
        // 保存时确认的实例已失去发现记录，而锁仍被占用；不能把未知启动中实例当成原目标。
        return Err(super::instance_changed());
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        // 有安全发现记录时不再访问旧磁盘来源；它可能已经被安装器移走或不可读。
        let source = if original.is_none() {
            Some(normalize_image(executable).ok_or_else(unverified)?)
        } else {
            None
        };
        let mut candidates = platform::lock_processes(home, &lock).map_err(failed)?;
        candidates.retain(|process| {
            if let Some(original) = &original {
                // 旧路径可能已被安装器移动；实例锁 + 同用户 PID 才是范围证明。
                // 发现文件发布后才创建的进程不能是其原实例，拒绝陈旧 PID 复用。
                process.pid() == original.pid
                    && process
                        .started_at()
                        .ok()
                        .zip(modified)
                        .is_some_and(|(start, modified)| start <= modified)
            } else {
                normalize_image(process.image()).as_ref() == source.as_ref()
            }
        });
        if candidates.len() != 1 {
            return Err(unverified());
        }
        let process = candidates.pop().expect("one verified candidate");
        // discovery 可在启动中首次发布，但只能属于已固定的原生进程。
        let current = optional_discovery(home)?;
        if original.is_some() {
            ensure_same_discovery(original.as_ref(), current.as_ref())?;
        } else if current.as_ref().is_some_and(|discovery| {
            discovery.pid != process.pid()
                || discovery
                    .executable_path
                    .as_deref()
                    .and_then(normalize_image)
                    .as_ref()
                    != source.as_ref()
        }) {
            return Err(super::instance_changed());
        }
        // 不要求旧磁盘摘要成立。平台层固定创建时间／句柄，并在结束前重验锁归属。
        process.terminate(home, &lock).map_err(failed)?;
        loop {
            let next = optional_discovery(home)?;
            if next.as_ref().is_some_and(|discovery| {
                discovery.pid != process.pid()
                    || current
                        .as_ref()
                        .or(original.as_ref())
                        .is_some_and(|current| {
                            discovery.instance_id != current.instance_id
                                || discovery.access_token != current.access_token
                        })
            }) {
                return Err(super::instance_changed());
            }
            if process.exited().map_err(failed)? {
                let fresh = open_lock(home).map_err(failed)?.ok_or_else(unverified)?;
                if !same_lock(&lock, &fresh)? {
                    return Err(super::instance_changed());
                }
                if available(&fresh)? {
                    return Ok(current.or(original).map(|discovery| discovery.instance_id));
                }
                // 进程对象已退出不表示所有内核文件清理已完成，继续在同一预算内等锁释放。
            }
            if Instant::now() >= deadline {
                return Err(failed(io::Error::new(io::ErrorKind::TimedOut, "exit wait")));
            }
            std::thread::sleep(super::POLL_INTERVAL);
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (deadline, executable);
        Err(unverified())
    }
}

fn optional_discovery(home: &Path) -> Result<Option<RuntimeDiscovery>, RuntimeBootstrapError> {
    optional_discovery_with_modified(home).map(|snapshot| snapshot.map(|(discovery, _)| discovery))
}

fn optional_discovery_with_modified(
    home: &Path,
) -> Result<Option<(RuntimeDiscovery, std::time::SystemTime)>, RuntimeBootstrapError> {
    match super::read_discovery_with_modified(home) {
        Ok((discovery, modified)) => {
            super::validate_discovery(&discovery)?;
            Ok(Some((discovery, modified)))
        }
        Err(_)
            if fs::symlink_metadata(home.join(super::DISCOVERY_RELATIVE_PATH))
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn ensure_same_discovery(
    expected: Option<&RuntimeDiscovery>,
    current: Option<&RuntimeDiscovery>,
) -> Result<(), RuntimeBootstrapError> {
    match (expected, current) {
        (Some(a), Some(b))
            if a.pid == b.pid
                && a.instance_id == b.instance_id
                && a.access_token == b.access_token
                && a.address == b.address
                && a.executable_path == b.executable_path =>
        {
            Ok(())
        }
        // 正常退出会移除原发现文件；没有后继记录时仍按原生身份判断。
        (Some(_), None) | (None, None) => Ok(()),
        _ => Err(super::instance_changed()),
    }
}

pub(super) fn open_lock(home: &Path) -> io::Result<Option<File>> {
    let path = home.join("run/runtime.lock");
    // 不为停止创建目录。存在的路径仍交给原生私有文件校验，不能跟随链接。
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::other("unsafe Runtime lock type"));
        }
        Ok(_) => {}
    }
    #[cfg(windows)]
    let file = super::windows::open_private_file(home, &path, true)?;
    #[cfg(not(windows))]
    let file = {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let mut parent = home.to_owned();
        for part in [None, Some("run")] {
            if let Some(part) = part {
                parent.push(part);
            }
            let metadata = fs::symlink_metadata(&parent)?;
            if !metadata.is_dir()
                || metadata.uid() != nix::unistd::geteuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err(io::Error::other("unsafe Runtime lock directory"));
            }
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::other("unsafe Runtime lock file"));
        }
        file
    };
    Ok(Some(file))
}

fn available(file: &File) -> Result<bool, RuntimeBootstrapError> {
    match file.try_lock() {
        Ok(()) => {
            file.unlock().map_err(failed)?;
            Ok(true)
        }
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(failed(error)),
    }
}

fn same_lock(a: &File, b: &File) -> Result<bool, RuntimeBootstrapError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let a = a.metadata().map_err(failed)?;
        let b = b.metadata().map_err(failed)?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        super::windows::same_lock(a, b).map_err(failed)
    }
}

fn normalize_image(path: &Path) -> Option<PathBuf> {
    crate::runtime_source::canonical_home(path)
}

fn unverified() -> RuntimeBootstrapError {
    super::bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeStopFailed,
        "无法证明目标 Runtime 的进程与实例锁归属，未结束任何进程。请重新查询后重试。",
    )
}

fn failed(error: io::Error) -> RuntimeBootstrapError {
    super::bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeStopFailed,
        format!("本机 Runtime 原生停止失败（系统错误：{error}）；未启动后继实例。"),
    )
}

#[cfg(test)]
mod tests;
