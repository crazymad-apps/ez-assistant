//! MSI 新包临时入口；不创建 WebView、Host 或 Runtime Home，不读取业务数据。
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ},
};

const IMAGES: [&str; 2] = ["ez-assistant-desktop.exe", "ez-assistant-runtime.exe"];

#[cfg(test)]
mod tests;

/// 返回 Some 时始终终止普通启动；参数错误不能退回 GUI 或启动 Host。
pub(crate) fn run_if_requested(config: &tauri::Config) -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mode = args.first()?.to_str()?;
    if !matches!(mode, "--prepare-installation" | "--verify-installation") {
        return None;
    }
    let result = (|| {
        let expected = if mode == "--prepare-installation" {
            2
        } else {
            3
        };
        if args.len() != expected {
            return Err(io::Error::other("invalid installation arguments"));
        }
        // 和正常 Desktop 初始化共用同一 mutex，整个同步操作在获取线程结束。
        let _startup = crate::desktop_update::windows::StartupGuard::acquire(&config.identifier)?;
        let target = install_path(Path::new(&args[1]))?;
        if mode == "--prepare-installation" {
            prepare(config, &target)
        } else {
            verify(&target)?;
            if args[2] != "." {
                verify(&install_path(Path::new(&args[2]))?)?;
            }
            Ok(())
        }
    })();
    match result {
        Ok(()) => Some(0),
        Err(error) => {
            // MSI 将非零视为失败；不输出 Token、Home 或账号信息。
            eprintln!("installation handoff failed: {error}");
            Some(1)
        }
    }
}

fn install_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().count() < 3 {
        return Err(io::Error::other("invalid installation directory"));
    }
    crate::runtime_source::canonical_home(path)
        .ok_or_else(|| io::Error::other("installation directory is unavailable"))
}

fn registered_installation(config: &tauri::Config) -> io::Result<Option<PathBuf>> {
    // 与 Tauri MSI manufacturer 缺省规则及本项目 WiX 注册位置一致，不接受调用方提供任意注册键。
    let publisher = config
        .bundle
        .publisher
        .as_deref()
        .or_else(|| config.identifier.split('.').nth(1))
        .ok_or_else(|| io::Error::other("missing installation publisher"))?;
    let product = config
        .product_name
        .as_deref()
        .ok_or_else(|| io::Error::other("missing product name"))?;
    let key = match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(format!(r"Software\{publisher}\{product}"), KEY_READ)
    {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match key.get_value::<OsString, _>("InstallDir") {
        Ok(path) => install_path(Path::new(&path)).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// MSI 安装已经获用户确认；核实注册事实后先 Desktop 后 Host，各阶段重新查询归属。
/// 只有程序进程被结束；不写文件/配置/数据库，不回滚执行任务，不启动旧版。
fn prepare(config: &tauri::Config, target: &Path) -> io::Result<()> {
    let previous = registered_installation(config)?;
    if previous.as_deref() != Some(target) && IMAGES.iter().any(|name| target.join(name).exists()) {
        return Err(io::Error::other(
            "target belongs to an unregistered installation",
        ));
    }
    if let Some(previous) = previous {
        // Desktop 真正退出后再查询 Host，包含它在退出前最后一次创建的后台进程。
        for name in IMAGES {
            let image = previous.join(name);
            for process in super::windows::installation_processes(&image)? {
                process.terminate_installation(&image)?;
                let deadline = Instant::now() + Duration::from_secs(30);
                while !process.exited()? {
                    if Instant::now() >= deadline {
                        return Err(io::Error::other("installation process did not exit"));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        verify(&previous)?;
    }
    verify(target)
}

fn verify(directory: &Path) -> io::Result<()> {
    for name in IMAGES.into_iter().chain([
        "ez-assistant-desktop-capture.exe",
        "runtime-tools/bin/rg.exe",
    ]) {
        super::windows::verify_installation_image(&directory.join(name))?;
    }
    // 只读系统现有延迟重命名事实；不把本安装的“重启后替换”当成本次安装成功。
    let key = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        r"SYSTEM\CurrentControlSet\Control\Session Manager",
        KEY_READ,
    )?;
    for name in [
        "PendingFileRenameOperations",
        "PendingFileRenameOperations2",
    ] {
        match key.get_value::<Vec<String>, _>(name) {
            Ok(paths) => verify_pending_paths(directory, &paths)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn verify_pending_paths(directory: &Path, paths: &[String]) -> io::Result<()> {
    for value in paths.iter().filter(|value| !value.is_empty()) {
        let value = value.trim_start_matches('!');
        let value = value.strip_prefix(r"\??\").unwrap_or(value);
        if crate::runtime_source::canonical_home(Path::new(value))
            .is_some_and(|path| path.starts_with(directory))
        {
            return Err(io::Error::other(
                "installation has a delayed file replacement",
            ));
        }
    }
    Ok(())
}
