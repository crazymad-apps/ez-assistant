//! PKG 的同步维护入口；不创建 WebView、启动 Host 或读写业务数据。
//! root 只确定系统用户、检查跨用户占用及调整本应用归属，停止在该用户身份下执行。

use super::{macos, native_stop};
use nix::unistd::{Uid, User, geteuid};
use std::{
    fs, io,
    io::Read as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const DESKTOP: &str = "ez-assistant-desktop";
const HOST: &str = "ez-assistant-runtime";

#[cfg(test)]
mod tests;

/// 所有维护参数（包括错误参数）均在插件和窗口初始化前结束，不能回落为普通启动。
pub(crate) fn run_if_requested(config: &tauri::Config) -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = args.first()?.to_str()?;
    if !matches!(
        mode,
        "--prepare-installation"
            | "--verify-installation"
            | "--finish-installation"
            | "--prepare-installation-user"
            | "--verify-installation-user"
    ) {
        return None;
    }
    let result = (|| {
        if args.len() != 3 {
            return Err(io::Error::other("invalid installation arguments"));
        }
        let target = installation_path(config, Path::new(&args[1]))?;
        let uid = fs::metadata("/dev/console")?.uid();
        let expected_uid = args[2].to_str().and_then(|value| value.parse::<u32>().ok());
        if uid < 501 || expected_uid != Some(uid) {
            return Err(io::Error::other("interactive installation user changed"));
        }
        let user = User::from_uid(Uid::from_raw(uid))?
            .ok_or_else(|| io::Error::other("installation user unavailable"))?;
        if geteuid().is_root() {
            // root 先检查所有目标映像；其他用户占用必须在任何终止和归属修改前拒绝。
            verify_user_scope(&target, uid)?;
            if mode == "--finish-installation" {
                validate_bundle(&target, &config.identifier)?;
                // PKG 的 root 归属会让现有登录前更新写检查失败。仅调整已核实的固定 App，
                // -P/-h 不跟随包内或顶层符号链接；系统目录和用户 Home 均不在递归范围。
                let status = Command::new("/usr/sbin/chown")
                    .args(["-R", "-P", "-h"])
                    .arg(format!("{}:{}", uid, user.gid))
                    .arg(&target)
                    .env_clear()
                    .status()?;
                if !status.success() {
                    return Err(io::Error::other("application ownership handoff failed"));
                }
            }
            let worker_mode = match mode {
                "--prepare-installation" => "--prepare-installation-user",
                "--verify-installation" | "--finish-installation" => "--verify-installation-user",
                _ => return Err(io::Error::other("user maintenance mode cannot run as root")),
            };
            // 使用系统 sudo 清理 supplementary groups 并切换身份，不在多线程 GUI 内降权，
            // 不执行用户 shell／profile，也不传递安装器的凭据和运行 Home 环境覆盖。
            // 系统脚本目录可能仅 root 可进入；复制签名字节到 root 持有的临时只读目录，
            // 不修改签名、不调用旧应用。父进程等待 worker 后才回收，不留下常驻 helper。
            let temporary = tempfile::Builder::new()
                .prefix("ez-install-")
                .tempdir_in("/private/tmp")?;
            let helper = temporary.path().join(DESKTOP);
            fs::copy(std::env::current_exe()?, &helper)?;
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
            fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755))?;
            let status = Command::new("/usr/bin/sudo")
                .args(["-n", "-H", "-u"])
                .arg(format!("#{uid}"))
                .arg("--")
                .arg(&helper)
                .arg(worker_mode)
                .arg(&target)
                .arg(uid.to_string())
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .current_dir("/")
                .status()?;
            if !status.success() {
                return Err(io::Error::other("installation user handoff failed"));
            }
            return Ok(());
        }
        if geteuid().as_raw() != uid || user.uid.as_raw() != uid {
            return Err(io::Error::other("installation user changed"));
        }
        match mode {
            "--prepare-installation" | "--prepare-installation-user" => prepare(&target, uid),
            "--verify-installation" | "--verify-installation-user" => {
                verify(&target, uid)?;
                for directory in [
                    &target,
                    target
                        .parent()
                        .ok_or_else(|| io::Error::other("missing installation parent"))?,
                ] {
                    tempfile::Builder::new()
                        .prefix(".ez-install-write-")
                        .tempfile_in(directory)?;
                }
                Ok(())
            }
            _ => Err(io::Error::other(
                "finishing installation requires system authorization",
            )),
        }
    })();
    match result {
        Ok(()) => {
            eprintln!("installation handoff completed: {mode}");
            Some(0)
        }
        Err(error) => {
            eprintln!("installation handoff failed: {mode}: {error}");
            Some(1)
        }
    }
}

fn installation_path(config: &tauri::Config, path: &Path) -> io::Result<PathBuf> {
    let product = config
        .product_name
        .as_deref()
        .ok_or_else(|| io::Error::other("missing product name"))?;
    if product.is_empty() || product.contains(['/', '\\']) {
        return Err(io::Error::other("invalid product name"));
    }
    let expected = Path::new("/Applications").join(format!("{product}.app"));
    if path != expected {
        return Err(io::Error::other("unexpected installation location"));
    }
    if fs::symlink_metadata(&expected).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::other("symbolic application target"));
    }
    if expected.exists() {
        validate_bundle(&expected, &config.identifier)?;
    }
    Ok(expected)
}

fn validate_bundle(path: &Path, identifier: &str) -> io::Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other("symbolic application target"));
    }
    let info_path = path.join("Contents/Info.plist");
    let metadata = fs::symlink_metadata(&info_path)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(io::Error::other("invalid application metadata"));
    }
    let mut bytes = Vec::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(info_path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("oversized application metadata"));
    }
    let info = plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(io::Error::other)?;
    if info
        .as_dictionary()
        .and_then(|info| info.get("CFBundleIdentifier"))
        .and_then(plist::Value::as_string)
        != Some(identifier)
    {
        return Err(io::Error::other("application identity mismatch"));
    }
    Ok(())
}

fn image(target: &Path, name: &str) -> PathBuf {
    target.join("Contents/MacOS").join(name)
}

fn verify_user_scope(target: &Path, uid: u32) -> io::Result<()> {
    for name in [DESKTOP, HOST] {
        macos::installation_processes(&image(target, name), uid)?;
    }
    Ok(())
}

/// 安装意图已确认；先 Desktop 后重新发现 Host，避免旧客户端在最后阶段重新启动后台。
/// 只结束当前安装的同用户进程；Home 从其实际锁 FD 确定，不读取或修复数据库。
fn prepare(target: &Path, uid: u32) -> io::Result<()> {
    verify_user_scope(target, uid)?;
    // 现有更新器需要同一用户可替换 App。权限不足在结束旧工作之前拒绝，
    // 不以一次 root 安装成功掩盖随后登录前无法更新的权限变化。
    tempfile::Builder::new()
        .prefix(".ez-install-write-")
        .tempfile_in(
            target
                .parent()
                .ok_or_else(|| io::Error::other("missing installation parent"))?,
        )?;
    for process in macos::installation_processes(&image(target, DESKTOP), uid)? {
        process.terminate_installation()?;
        wait_exit(&process)?;
    }
    for process in macos::installation_processes(&image(target, HOST), uid)? {
        let homes = process.runtime_homes()?;
        if homes.is_empty() {
            // 尚未获取实例锁的早期进程仍以固定映像、owner 和创建时间核对，不要求 HTTP。
            process.terminate_installation()?;
            wait_exit(&process)?;
        } else {
            for home in homes {
                let discovery = match super::read_discovery(&home) {
                    Ok(discovery) => {
                        if discovery.pid != process.pid() {
                            return Err(io::Error::other("installation Runtime instance changed"));
                        }
                        Some(discovery)
                    }
                    Err(_) if !home.join(super::DISCOVERY_RELATIVE_PATH).exists() => None,
                    Err(error) => return Err(io::Error::other(error.to_string())),
                };
                // 复用既有进程退出 + 同一锁释放的预算；不重建单次 flock 的探针漏洞。
                native_stop::stop(&home, process.image(), discovery.as_ref())
                    .map_err(|error| io::Error::other(error.to_string()))?;
            }
        }
    }
    verify(target, uid)
}

fn wait_exit(process: &macos::LockProcess) -> io::Result<()> {
    let deadline = Instant::now() + super::SHUTDOWN_TIMEOUT;
    while !process.exited()? {
        if Instant::now() >= deadline {
            return Err(io::Error::other("installation process exit timed out"));
        }
        std::thread::sleep(super::POLL_INTERVAL);
    }
    Ok(())
}

fn verify(target: &Path, uid: u32) -> io::Result<()> {
    for name in [DESKTOP, HOST] {
        if !macos::installation_processes(&image(target, name), uid)?.is_empty() {
            return Err(io::Error::other("installation still in use"));
        }
    }
    Ok(())
}
