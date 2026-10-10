//! 原生更新操作的唯一 owner；WebView 只读状态并发出开始/取消意图。
mod download;
mod install;
mod manifest;
mod schedule;
mod source;
#[cfg(test)]
mod tests;
#[cfg(windows)]
pub(crate) mod windows;

use assistant_protocol::{
    ClientCompatibility, HostMode, RuntimeHostCapabilities, SOFTWARE_VERSION,
};
use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter as _, State};
use tauri_plugin_updater::{Update, UpdaterExt as _};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::runtime_bootstrap::RuntimeBootstrap;

const CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const PUBLIC_KEY: &str = include_str!(concat!(env!("OUT_DIR"), "/desktop-update.pub"));

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error("暂时无法检查更新，请稍后重试。")]
    Check,
    #[error("更新文件校验失败，请稍后重试。")]
    InvalidPackage,
    #[error("下载未完成，请稍后重试。")]
    Download,
    #[error("更新已取消。")]
    Cancelled,
    #[error("正在安装更新，请稍候。")]
    Installing,
    #[error("暂时无法更新，请稍后重试。")]
    Unavailable,
    #[error("无法完成更新。请重新打开已安装的应用后重试。")]
    Install,
    #[error("请将应用移到可写的安装位置后重试。")]
    ReadOnly,
}

impl Serialize for UpdateError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Error {
            message: String,
        }
        Error {
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

#[derive(Clone)]
struct UpdateSource {
    binding: String,
    center: Url,
    host: ClientCompatibility,
    protocols: Vec<u32>,
    local: Option<RuntimeBootstrap>,
    remote_origin: Option<String>,
    cancellation: CancellationToken,
}

#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    #[default]
    Idle,
    Available,
    Downloading,
    Installing,
    Failed,
}

#[derive(Clone, Default, Serialize)]
pub(crate) struct UpdateSnapshot {
    revision: u64,
    phase: Phase,
    available: bool,
    required: bool,
    notes: String,
    received: u64,
    total: u64,
    error: Option<String>,
    resume_remote: Option<String>,
    stops_local_host: bool,
    binding_id: Option<String>,
    host_mode: Option<HostMode>,
}

#[derive(Clone)]
struct Candidate {
    update: Update,
    manifest: manifest::VerifiedManifest,
}

#[derive(Default)]
struct Inner {
    snapshot: UpdateSnapshot,
    source: Option<UpdateSource>,
    candidate: Option<Candidate>,
    recovery_error: Option<String>,
}
struct Job {
    cancellation: CancellationToken,
    handle: tauri::async_runtime::JoinHandle<()>,
}
#[derive(Default)]
struct Jobs {
    recovery: Option<Job>,
    check: Option<Job>,
    operation: Option<Job>,
}

#[derive(Clone, Default)]
pub(crate) struct DesktopUpdater {
    inner: Arc<Mutex<Inner>>,
    jobs: Arc<tokio::sync::Mutex<Jobs>>,
}

pub(crate) struct LifecyclePermit {
    _guard: tokio::sync::OwnedMutexGuard<Jobs>,
}

impl DesktopUpdater {
    pub(crate) fn startup_home(app: &AppHandle) -> Option<std::path::PathBuf> {
        install::startup_home(app)
    }
    /// 普通启停与进入安装阶段串行，防止等待中的启停意图在安装失败后误启动旧 Host。
    pub(crate) async fn lifecycle_permit(&self) -> Result<LifecyclePermit, UpdateError> {
        let guard = self.jobs.clone().lock_owned().await;
        if self.installing() {
            return Err(UpdateError::Installing);
        }
        Ok(LifecyclePermit { _guard: guard })
    }
    pub(crate) fn initialize(&self, app: &AppHandle) {
        let Ok(mut jobs) = self.jobs.try_lock() else {
            return;
        };
        let owner = self.clone();
        let app = app.clone();
        let cancellation = CancellationToken::new();
        let stop = cancellation.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let recovery = tokio::select! {
                biased;
                () = stop.cancelled() => return,
                result = install::recover(&app) => result,
            };
            owner.publish(&app, |inner| match recovery {
                Ok(origin) => {
                    inner.snapshot.resume_remote = origin;
                }
                Err(error) => {
                    inner.snapshot.phase = Phase::Failed;
                    inner.snapshot.error = Some(error.to_string());
                    inner.recovery_error = inner.snapshot.error.clone();
                }
            });
        });
        jobs.recovery = Some(Job {
            cancellation,
            handle,
        });
    }

    pub(crate) async fn shutdown(&self) -> Result<(), UpdateError> {
        let mut jobs = self.jobs.lock().await;
        if self.installing() {
            return Err(UpdateError::Installing);
        }
        Self::finish(&mut jobs.recovery).await;
        Self::finish(&mut jobs.check).await;
        Self::finish(&mut jobs.operation).await;
        Ok(())
    }
    pub(crate) fn installing(&self) -> bool {
        self.inner
            .lock()
            .is_ok_and(|inner| inner.snapshot.phase == Phase::Installing)
    }

    fn publish(&self, app: &AppHandle, change: impl FnOnce(&mut Inner)) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        change(&mut inner);
        inner.snapshot.revision += 1;
        let snapshot = inner.snapshot.clone();
        drop(inner);
        let _ = app.emit("desktop-update-state", snapshot);
    }

    pub(crate) fn snapshot(&self) -> UpdateSnapshot {
        self.inner
            .lock()
            .map(|inner| inner.snapshot.clone())
            .unwrap_or_default()
    }

    /// 只接受当前已选 Host 的明确拒绝；网络、身份及旧接口缺失不会调用此入口。
    pub(crate) fn require_update(&self, app: &AppHandle, binding: &str) {
        self.publish(app, |inner| {
            if inner.snapshot.binding_id.as_deref() == Some(binding) {
                inner.snapshot.required = true;
            }
        });
    }

    pub(crate) fn update_required(&self, binding: &str) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            inner.snapshot.binding_id.as_deref() == Some(binding) && inner.snapshot.required
        })
    }

    async fn ready(&self) {
        let mut jobs = self.jobs.lock().await;
        if let Some(recovery) = jobs.recovery.take() {
            let _ = recovery.handle.await;
        }
    }

    async fn finish(job: &mut Option<Job>) {
        if let Some(job) = job.take() {
            job.cancellation.cancel();
            let _ = job.handle.await;
        }
    }

    pub(crate) async fn replace_connection<T>(
        &self,
        app: &AppHandle,
        begin: impl FnOnce() -> T,
    ) -> Result<T, UpdateError> {
        let mut jobs = self.jobs.lock().await;
        if let Some(recovery) = jobs.recovery.take() {
            let _ = recovery.handle.await;
        }
        if self.installing() {
            return Err(UpdateError::Installing);
        }
        // 先在同一生命周期锁内取消旧连接，再清空更新状态，阻止迟到的登录前探测写回。
        let selected = begin();
        Self::finish(&mut jobs.check).await;
        Self::finish(&mut jobs.operation).await;
        self.publish(app, |inner| {
            let revision = inner.snapshot.revision;
            let recovery_error = inner.recovery_error.clone();
            *inner = Inner::default();
            inner.snapshot.revision = revision;
            inner.recovery_error = recovery_error.clone();
            if recovery_error.is_some() {
                inner.snapshot.phase = Phase::Failed;
                inner.snapshot.error = recovery_error;
            }
        });
        Ok(selected)
    }

    /// 从 Host 获取并冻结来源；登录失败不会清除它，来源变化则收束旧任务。
    pub(crate) async fn prepare(
        &self,
        app: &AppHandle,
        binding: String,
        origin: &str,
        capabilities: &RuntimeHostCapabilities,
        local: Option<RuntimeBootstrap>,
        cancellation: CancellationToken,
    ) {
        if self.inner.lock().is_ok_and(|inner| {
            !inner.snapshot.required
                && inner.source.as_ref().is_some_and(|source| {
                    source.binding == binding
                        && (source.remote_origin.as_deref() == Some(origin)
                            || source
                                .local
                                .as_ref()
                                .is_some_and(|local| local.base_url == origin))
                })
        }) {
            return;
        }
        // 保留选中目标的最小投影，WebView 刷新可复用登录前检查和下载。
        // 即使旧 Host 没有更新上下文，后续明确的不兼容错误仍归属这个目标。
        if cancellation.is_cancelled() {
            return;
        }
        self.publish(app, |inner| {
            if cancellation.is_cancelled() {
                return;
            }
            inner.snapshot.binding_id = Some(binding.clone());
            inner.snapshot.host_mode = Some(capabilities.mode);
            inner.snapshot.resume_remote = local.is_none().then(|| origin.to_owned());
            inner.snapshot.stops_local_host = local.is_some();
        });
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            result = source::read(binding, origin, capabilities, local, cancellation.clone()) => result,
        };
        let Ok(Some(source)) = result else {
            return;
        };
        let mut jobs = self.jobs.lock().await;
        if self.installing() || source.cancellation.is_cancelled() {
            return;
        }
        // 同一来源已开始的下载不因重复探测重启。
        if jobs
            .operation
            .as_ref()
            .is_some_and(|job| !job.handle.inner().is_finished())
        {
            return;
        }
        Self::finish(&mut jobs.check).await;
        self.publish(app, |inner| {
            inner.snapshot.stops_local_host = source.local.is_some();
            inner.source = Some(source.clone());
        });
        let owner = self.clone();
        let app = app.clone();
        let cancellation = source.cancellation.child_token();
        let stop = cancellation.clone();
        let handle = tauri::async_runtime::spawn(async move {
            schedule::poll(&stop, || owner.check(&app, &source)).await;
        });
        jobs.check = Some(Job {
            cancellation,
            handle,
        });
    }

    async fn check(&self, app: &AppHandle, source: &UpdateSource) {
        let protocol = match source.center_protocol().await {
            Ok(protocol) => protocol,
            Err(_) => {
                self.clear_candidate(app, source);
                return;
            }
        };
        self.publish(app, |inner| {
            if active(inner, source) {
                inner.snapshot.required = !source.supports(protocol);
            }
        });
        if matches!(
            self.snapshot().phase,
            Phase::Downloading | Phase::Installing
        ) {
            return;
        }
        let result = self.candidate(app, source, protocol).await;
        self.publish(app, |inner| {
            if !active(inner, source)
                || matches!(inner.snapshot.phase, Phase::Downloading | Phase::Installing)
            {
                return;
            }
            inner.candidate = result.ok().flatten();
            inner.snapshot.available = inner.candidate.is_some();
            inner.snapshot.notes = inner
                .candidate
                .as_ref()
                .map(|c| c.manifest.notes.clone())
                .unwrap_or_default();
            if inner.snapshot.phase != Phase::Failed {
                inner.snapshot.phase = if inner.snapshot.available {
                    Phase::Available
                } else {
                    Phase::Idle
                };
            }
        });
    }

    fn clear_candidate(&self, app: &AppHandle, source: &UpdateSource) {
        self.publish(app, |inner| {
            if active(inner, source)
                && !matches!(inner.snapshot.phase, Phase::Downloading | Phase::Installing)
            {
                inner.candidate = None;
                inner.snapshot.available = false;
                if inner.snapshot.phase == Phase::Available {
                    inner.snapshot.phase = Phase::Idle;
                }
            }
        });
    }

    async fn candidate(
        &self,
        app: &AppHandle,
        source: &UpdateSource,
        protocol: (u32, u32),
    ) -> Result<Option<Candidate>, UpdateError> {
        let target = target().ok_or(UpdateError::Unavailable)?;
        let key = manifest::public_key(PUBLIC_KEY)?;
        let mut endpoint = source.endpoint(&format!("{target}/{SOFTWARE_VERSION}"))?;
        if source.local.is_none() {
            endpoint
                .query_pairs_mut()
                .append_pair("host_version", &source.host.version)
                .append_pair(
                    "host_min_supported_version",
                    &source.host.min_compatible_version,
                )
                .append_pair(
                    "host_protocols",
                    &source
                        .protocols
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                );
        }
        let update = app
            .updater_builder()
            .target(target)
            .pubkey(PUBLIC_KEY.trim())
            .endpoints(vec![endpoint])
            .map_err(|_| UpdateError::Check)?
            .timeout(Duration::from_secs(5))
            .clear_headers()
            .configure_client(|builder| builder.redirect(reqwest::redirect::Policy::none()))
            .build()
            .map_err(|_| UpdateError::Check)?
            .check()
            .await
            .map_err(|_| UpdateError::Check)?;
        let Some(update) = update else {
            return Ok(None);
        };
        let manifest = manifest::VerifiedManifest::read(
            &update.raw_json,
            &key,
            source,
            target,
            SOFTWARE_VERSION,
            protocol,
        )?;
        if update.version != manifest.version
            || update.download_url != manifest.url
            || Some(&update.signature) != manifest.artifact.signature.as_ref()
        {
            return Err(UpdateError::InvalidPackage);
        }
        Ok(Some(Candidate { update, manifest }))
    }

    async fn start(&self, app: &AppHandle) -> Result<(), UpdateError> {
        let mut jobs = self.jobs.lock().await;
        if jobs
            .operation
            .as_ref()
            .is_some_and(|job| !job.handle.inner().is_finished())
        {
            return Ok(());
        }
        Self::finish(&mut jobs.operation).await;
        let (source, candidate) = {
            let inner = self.inner.lock().map_err(|_| UpdateError::Unavailable)?;
            (
                inner.source.clone().ok_or(UpdateError::Unavailable)?,
                inner.candidate.clone().ok_or(UpdateError::Unavailable)?,
            )
        };
        if source.cancellation.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        let cancellation = source.cancellation.child_token();
        let stop = cancellation.clone();
        self.publish(app, |inner| {
            inner.snapshot.phase = Phase::Downloading;
            inner.snapshot.error = None;
            inner.snapshot.received = 0;
            inner.snapshot.total = candidate.manifest.artifact.size;
        });
        let app = app.clone();
        let owner = self.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let result = install::run(&owner, &app, &source, candidate, &stop).await;
            owner.publish(&app, |inner| {
                if !active(inner, &source) {
                    return;
                }
                match result {
                    Err(UpdateError::Cancelled) => {
                        inner.snapshot.phase = if inner.snapshot.available {
                            Phase::Available
                        } else {
                            Phase::Idle
                        };
                        inner.snapshot.error = None;
                    }
                    Err(error) => {
                        inner.snapshot.phase = Phase::Failed;
                        inner.snapshot.error = Some(error.to_string());
                    }
                    Ok(()) => {}
                }
            });
        });
        jobs.operation = Some(Job {
            cancellation,
            handle,
        });
        Ok(())
    }

    async fn cancel(&self) -> Result<(), UpdateError> {
        let mut jobs = self.jobs.lock().await;
        if self.installing() {
            return Err(UpdateError::Installing);
        }
        Self::finish(&mut jobs.operation).await;
        Ok(())
    }
}

fn active(inner: &Inner, source: &UpdateSource) -> bool {
    !source.cancellation.is_cancelled()
        && inner
            .source
            .as_ref()
            .is_some_and(|s| s.binding == source.binding && s.center == source.center)
}

fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("windows", "aarch64") => Some("windows-aarch64"),
        _ => None,
    }
}

#[tauri::command]
pub(crate) async fn desktop_update_state(
    owner: State<'_, DesktopUpdater>,
) -> Result<UpdateSnapshot, UpdateError> {
    owner.ready().await;
    Ok(owner.snapshot())
}

#[tauri::command]
pub(crate) async fn start_desktop_update(
    app: AppHandle,
    owner: State<'_, DesktopUpdater>,
) -> Result<(), UpdateError> {
    owner.start(&app).await
}

#[tauri::command]
pub(crate) async fn cancel_desktop_update(
    owner: State<'_, DesktopUpdater>,
) -> Result<(), UpdateError> {
    owner.cancel().await
}
