//! 下载完成后才进入不可取消的进程交接；恢复记录不保存凭据。
use assistant_protocol::SOFTWARE_VERSION;
use serde::{Deserialize, Serialize};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager as _};
use tokio_util::sync::CancellationToken;

use super::{
    Candidate, DesktopUpdater, PUBLIC_KEY, Phase, UpdateError, UpdateSource, download, manifest,
    source, target,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingUpdate {
    previous_version: String,
    expected_version: String,
    artifact_sha256: String,
    runtime_home: Option<PathBuf>,
    remote_origin: Option<String>,
}

fn marker_path(app: &AppHandle) -> Result<PathBuf, UpdateError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| UpdateError::Install)?;
    std::fs::create_dir_all(&directory).map_err(|_| UpdateError::Install)?;
    Ok(directory.join("pending-update.json"))
}

fn write_marker(path: &Path, record: &PendingUpdate) -> Result<(), UpdateError> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or(UpdateError::Install)?)
        .map_err(|_| UpdateError::Install)?;
    serde_json::to_writer(file.as_file_mut(), record).map_err(|_| UpdateError::Install)?;
    file.flush().map_err(|_| UpdateError::Install)?;
    file.as_file()
        .sync_all()
        .map_err(|_| UpdateError::Install)?;
    file.persist(path).map_err(|_| UpdateError::Install)?;
    Ok(())
}

fn writable_installation() -> Result<(), UpdateError> {
    #[cfg(target_os = "macos")]
    {
        let executable = std::env::current_exe().map_err(|_| UpdateError::ReadOnly)?;
        let bundle = executable
            .ancestors()
            .find(|p| p.extension().is_some_and(|ext| ext == "app"))
            .ok_or(UpdateError::ReadOnly)?;
        for directory in [bundle, bundle.parent().ok_or(UpdateError::ReadOnly)?] {
            tempfile::Builder::new()
                .prefix(".ez-update-check-")
                .tempfile_in(directory)
                .map_err(|_| UpdateError::ReadOnly)?;
        }
    }
    Ok(())
}

pub(super) async fn run(
    owner: &DesktopUpdater,
    app: &AppHandle,
    source: &UpdateSource,
    candidate: Candidate,
    cancellation: &CancellationToken,
) -> Result<(), UpdateError> {
    let key = manifest::public_key(PUBLIC_KEY)?;
    let http = source::http()?;
    let file = download::download(
        &http,
        &candidate.manifest,
        &key,
        cancellation,
        |received, total| {
            owner.publish(app, |inner| {
                inner.snapshot.received = received;
                inner.snapshot.total = total;
            });
        },
    )
    .await?;
    // 已撤回或同一路径被替换的候选不可在停止 Host 后才发现。
    tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(UpdateError::Cancelled),
        result = async {
            let target = target().ok_or(UpdateError::Unavailable)?;
            let protocol = source.center_protocol().await?;
            let response = http.get(source.endpoint(&format!("{target}/releases/{}", candidate.manifest.version))?)
                .timeout(Duration::from_secs(5)).send().await.map_err(|_| UpdateError::Check)?;
            let json = source::json(response, 256 * 1024).await?;
            let latest = manifest::VerifiedManifest::read(&json, &key, source, target, SOFTWARE_VERSION, protocol)?;
            if latest.manifest_sha256 != candidate.manifest.manifest_sha256 { return Err(UpdateError::InvalidPackage); }
            Ok(())
        } => result?,
    }
    writable_installation()?;
    // 把磁盘读取/内存分配失败留在停止 Host 之前；阻塞读取由当前操作等待收束。
    let bytes = tokio::task::spawn_blocking(move || file.into_bytes())
        .await
        .map_err(|_| UpdateError::Download)??;
    let marker = marker_path(app)?;
    // 与取消、切换连接共同串行。等待时仍可取消，避免退出等待自身任务。
    let jobs = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(UpdateError::Cancelled),
        guard = owner.jobs.lock() => guard,
    };
    if cancellation.is_cancelled() {
        return Err(UpdateError::Cancelled);
    }
    owner.publish(app, |inner| {
        inner.snapshot.phase = Phase::Installing;
    });
    drop(jobs);
    let runtime = app
        .state::<crate::runtime_bootstrap::RuntimeBootstrapCoordinator>()
        .inner()
        .clone();
    let handoff = runtime
        .desktop_handoff(source.local.as_ref())
        .await
        .map_err(|_| UpdateError::Install)?;
    write_marker(
        &marker,
        &PendingUpdate {
            previous_version: SOFTWARE_VERSION.into(),
            expected_version: candidate.manifest.version.clone(),
            artifact_sha256: candidate.manifest.artifact.sha256.clone(),
            runtime_home: handoff.runtime_home(),
            remote_origin: source.remote_origin.clone(),
        },
    )?;
    handoff.stop().await.map_err(|error| {
        eprintln!("desktop update: Host handoff failed: {error}");
        UpdateError::Install
    })?;
    app.state::<crate::browser_resource::BrowserResourceManager>()
        .close_all();
    app.state::<crate::native_resource::NativeResourceBridge>()
        .clear_target_resources();
    app.state::<crate::system_notification::SystemNotifications>()
        .cancel_all();
    let update = candidate.update;
    tokio::task::spawn_blocking(move || {
        update.install(bytes).map_err(|error| {
            eprintln!("desktop update: installation failed: {error}");
            UpdateError::Install
        })
    })
    .await
    .map_err(|_| UpdateError::Install)??;
    // Windows 成功启动安装器后已退出；macOS 在原位替换成功后重启。
    drop(handoff);
    app.state::<crate::desktop_lifecycle::DesktopLifecycleCoordinator>()
        .allow_exit();
    app.restart();
}

fn read_marker(app: &AppHandle) -> Result<Option<PendingUpdate>, UpdateError> {
    let path = marker_path(app)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        result => result.map_err(|_| UpdateError::Install)?,
    };
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(UpdateError::Install);
    }
    let record: PendingUpdate =
        serde_json::from_slice(&std::fs::read(&path).map_err(|_| UpdateError::Install)?)
            .map_err(|_| UpdateError::Install)?;
    if record.artifact_sha256.len() != 64
        || !record
            .artifact_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || record.runtime_home.is_some() == record.remote_origin.is_some()
    {
        return Err(UpdateError::Install);
    }
    if record
        .runtime_home
        .as_ref()
        .is_some_and(|home| !home.is_absolute() || !home.is_dir())
    {
        return Err(UpdateError::Install);
    }
    if let Some(origin) = &record.remote_origin {
        let url = url::Url::parse(origin).map_err(|_| UpdateError::Install)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(UpdateError::Install);
        }
    }
    Ok(Some(record))
}

/// 安装器重启可能不保留原环境变量，所有后续本机操作也必须采用恢复记录的同一 Home。
pub(super) fn startup_home(app: &AppHandle) -> Option<PathBuf> {
    match read_marker(app) {
        Ok(Some(record)) => record.runtime_home,
        Ok(None) => None,
        // 损坏记录不能退回默认 Home 后自动启动一个无关 Host。
        Err(_) => Some(PathBuf::new()),
    }
}

pub(super) async fn recover(app: &AppHandle) -> Result<Option<String>, UpdateError> {
    let Some(record) = read_marker(app)? else {
        return Ok(None);
    };
    if let Some(origin) = &record.remote_origin {
        app.state::<DesktopUpdater>().publish(app, |inner| {
            inner.snapshot.resume_remote = Some(origin.clone());
        });
    }
    if record.expected_version != SOFTWARE_VERSION || record.previous_version == SOFTWARE_VERSION {
        return Err(UpdateError::Install);
    }
    if let Some(home) = record.runtime_home {
        if !home.is_absolute() {
            return Err(UpdateError::Install);
        }
        app.state::<crate::runtime_bootstrap::RuntimeBootstrapCoordinator>()
            .resume_desktop_update(&home)
            .await
            .map_err(|_| UpdateError::Install)?;
    }
    std::fs::remove_file(marker_path(app)?).map_err(|_| UpdateError::Install)?;
    // 安装器重启后显式恢复主窗口，不能沿用安装退出时的隐藏/最小化状态。
    crate::desktop_lifecycle::show_desktop_window(app.clone()).map_err(|_| UpdateError::Install)?;
    Ok(record.remote_origin)
}
