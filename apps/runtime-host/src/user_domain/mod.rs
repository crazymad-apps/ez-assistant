//! Host 私有用户服务缓存。初始化和关闭共用用户锁，所有异步装配均由 Host 子系统监督。

mod services;

use crate::{
    access::{AccessError, AccessPermit, Credentials, enterprise::UserKey},
    config::ServeConfig,
    config_source::LocalConfigSource,
    http::{ReadyServices, StartupStateHandle},
    supervisor::{FailurePolicy, HostSupervisor},
};
use assistant_protocol::RuntimeHostStartupError;
use futures_util::FutureExt;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, OwnedMutexGuard, mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

type Key = Option<UserKey>;
type Slot = Arc<AsyncMutex<Option<Arc<LoadedDomain>>>>;

struct LoadedDomain {
    services: Arc<ReadyServices>,
    access: CancellationToken,
    closed: watch::Receiver<Option<bool>>,
}

/// 即使用户监督任务 panic，缓存也不能继续返回无 owner 的 Runtime。
struct DomainCompletion {
    access: CancellationToken,
    closed: watch::Sender<Option<bool>>,
}
impl Drop for DomainCompletion {
    fn drop(&mut self) {
        self.access.cancel();
        if self.closed.borrow().is_none() {
            self.closed.send_replace(Some(false));
        }
    }
}

struct OpenDomain {
    key: Key,
    permit: AccessPermit,
    slot: Slot,
    guard: OwnedMutexGuard<Option<Arc<LoadedDomain>>>,
    reply: oneshot::Sender<Result<Arc<ReadyServices>, DomainError>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DomainError {
    #[error(transparent)]
    Access(#[from] AccessError),
    #[error("用户数据初始化失败：{0:?}")]
    Initialization(RuntimeHostStartupError),
    #[error("用户服务未能完成关闭，请重启 Host 后重试。")]
    Shutdown,
}

pub(crate) struct UserDomains {
    slots: Mutex<HashMap<Key, Slot>>,
    open: mpsc::UnboundedSender<OpenDomain>,
    stopping: CancellationToken,
    stopped: watch::Receiver<Option<bool>>,
}

impl UserDomains {
    pub(crate) async fn wait_shutdown(&self) -> Result<(), DomainError> {
        let mut stopped = self.stopped.clone();
        match stopped.wait_for(Option::is_some).await {
            Ok(result) if *result == Some(true) => Ok(()),
            _ => Err(DomainError::Shutdown),
        }
    }
    pub(crate) async fn finish_logout(&self, key: Option<&UserKey>) -> Result<(), DomainError> {
        let slot = self
            .slots
            .lock()
            .expect("user domains poisoned")
            .get(&key.cloned())
            .cloned();
        let Some(slot) = slot else {
            return Ok(());
        };
        let guard = slot.lock().await;
        let Some(domain) = guard.as_ref().filter(|domain| domain.access.is_cancelled()) else {
            return Ok(());
        };
        let mut closed = domain.closed.clone();
        drop(guard);
        match closed.wait_for(Option::is_some).await {
            Ok(result) if *result == Some(true) => Ok(()),
            _ => Err(DomainError::Shutdown),
        }
    }

    /// 只排队一次实际初始化；随后等待者在同一用户锁上复用结果。
    pub(crate) async fn ensure(
        &self,
        permit: &AccessPermit,
    ) -> Result<Arc<ReadyServices>, DomainError> {
        let key = permit.user_key().cloned();
        let slot = self
            .slots
            .lock()
            .expect("user domains poisoned")
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone();
        loop {
            let guard = tokio::select! {
                biased;
                () = self.stopping.cancelled() => return Err(DomainError::Shutdown),
                () = permit.ended() => return Err(permit.check().err().unwrap_or(AccessError::Unauthorized).into()),
                guard = slot.clone().lock_owned() => guard,
            };
            permit.check()?;
            if let Some(loaded) = guard.as_ref() {
                if !loaded.access.is_cancelled() {
                    return Ok(loaded.services.clone());
                }
                // 关闭失败仍保留原 owner，不能开第二个 Store；成功后才释放缓存。
                let mut closed = loaded.closed.clone();
                drop(guard);
                let result = tokio::select! {
                    () = permit.ended() => return Err(permit.check().err().unwrap_or(AccessError::Unauthorized).into()),
                    () = self.stopping.cancelled() => return Err(DomainError::Shutdown),
                    result = closed.wait_for(Option::is_some) => result.map(|v| *v).unwrap_or(Some(false)),
                };
                if result == Some(false) {
                    return Err(DomainError::Shutdown);
                }
                continue;
            }
            // 锁被转交给受监督任务，HTTP 断开不会释放初始化互斥或丢失资源收尾。
            let (reply, result) = oneshot::channel();
            self.open
                .send(OpenDomain {
                    key,
                    permit: permit.clone(),
                    slot: slot.clone(),
                    guard,
                    reply,
                })
                .map_err(|_| DomainError::Shutdown)?;
            return tokio::select! {
                () = self.stopping.cancelled() => Err(DomainError::Shutdown),
                () = permit.ended() => Err(permit.check().err().unwrap_or(AccessError::Unauthorized).into()),
                result = result => {
                    permit.check()?;
                    result.map_err(|_| DomainError::Shutdown)?
                }
            };
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct SharedResources {
    pub(crate) protected_files: Arc<std::sync::RwLock<Vec<std::path::PathBuf>>>,
    speech_slots: crate::speech::SpeechSlots,
}

pub(crate) struct UserDomainService {
    pub(crate) shared: SharedResources,
    config: ServeConfig,
    credentials: Arc<Credentials>,
    startup: StartupStateHandle,
    enterprise: bool,
    #[cfg(test)]
    pub(crate) model: Option<Arc<dyn assistant_runtime::ModelServiceFactory>>,
    domains: Arc<UserDomains>,
    requests: mpsc::UnboundedReceiver<OpenDomain>,
    stopped: watch::Sender<Option<bool>>,
}

impl UserDomainService {
    pub(crate) fn new(
        config: ServeConfig,
        credentials: Arc<Credentials>,
        startup: StartupStateHandle,
        enterprise: bool,
    ) -> (Self, Arc<UserDomains>) {
        let (open, requests) = mpsc::unbounded_channel();
        let (stopped, completion) = watch::channel(None);
        let domains = Arc::new(UserDomains {
            slots: Mutex::new(HashMap::new()),
            open,
            stopping: CancellationToken::new(),
            stopped: completion,
        });
        (
            Self {
                shared: SharedResources::default(),
                config,
                credentials,
                startup,
                enterprise,
                #[cfg(test)]
                model: None,
                domains: domains.clone(),
                requests,
                stopped,
            },
            domains,
        )
    }

    pub(crate) async fn run_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), std::io::Error> {
        let mut tasks = JoinSet::new();
        let mut failed = false;
        if !self.enterprise {
            let slot = self
                .domains
                .slots
                .lock()
                .expect("user domains poisoned")
                .entry(None)
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone();
            if let Ok(guard) = slot.clone().try_lock_owned() {
                let (reply, _) = oneshot::channel();
                let request = OpenDomain {
                    key: None,
                    permit: AccessPermit::new(true, None, shutdown.clone()),
                    guard,
                    slot,
                    reply,
                };
                self.spawn(&mut tasks, request, shutdown.clone());
            }
        }
        let mut changes = self.credentials.subscribe_changes();
        loop {
            self.prune_unloaded();
            let deadline = self.credentials.next_login_deadline();
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                Some(request) = self.requests.recv() => self.spawn(&mut tasks, request, shutdown.clone()),
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if !matches!(result, Ok(true)) { failed = true; eprintln!("runtime-host: user domain cleanup failed"); }
                }
                _ = changes.changed() => {},
                () = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => {},
            }
        }
        self.domains.stopping.cancel();
        self.requests.close();
        // 尚未装配的请求只退还锁；已运行的用户任务并行关闭，不按用户累加期限。
        while let Some(request) = self.requests.recv().await {
            let _ = request.reply.send(Err(DomainError::Shutdown));
        }
        let drained = tokio::time::timeout(Duration::from_secs(14), async {
            while let Some(result) = tasks.join_next().await {
                if !matches!(result, Ok(true)) {
                    failed = true;
                    eprintln!("runtime-host: user domain close failed");
                }
            }
        })
        .await;
        if drained.is_err() {
            eprintln!("runtime-host: user domains did not converge before Host shutdown deadline");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            self.stopped.send_replace(Some(false));
            return Err(std::io::Error::other("user domain shutdown timed out"));
        }
        self.stopped.send_replace(Some(!failed));
        if failed {
            return Err(std::io::Error::other("user domain cleanup failed"));
        }
        Ok(())
    }

    fn prune_unloaded(&self) {
        for key in self.credentials.enterprise_users() {
            let slot = self
                .domains
                .slots
                .lock()
                .expect("user domains poisoned")
                .get(&Some(key.clone()))
                .cloned();
            let unused = slot
                .as_ref()
                .is_none_or(|slot| slot.try_lock().is_ok_and(|guard| guard.is_none()));
            if unused {
                self.credentials.end_unused_user(&key);
            }
        }
    }

    fn spawn(&self, tasks: &mut JoinSet<bool>, request: OpenDomain, shutdown: CancellationToken) {
        let config = self.config.clone();
        let shared = self.shared.clone();
        let credentials = self.credentials.clone();
        let startup = if request.key.is_none() {
            self.startup.clone()
        } else {
            StartupStateHandle::new()
        };
        #[cfg(test)]
        let model = self.model.clone();
        tasks.spawn(async move {
            run_domain(
                config,
                shared,
                credentials,
                startup,
                request,
                shutdown,
                #[cfg(test)]
                model,
            )
            .await
        });
    }
}

async fn run_domain(
    config: ServeConfig,
    shared: SharedResources,
    credentials: Arc<Credentials>,
    startup: StartupStateHandle,
    request: OpenDomain,
    shutdown: CancellationToken,
    #[cfg(test)] model: Option<Arc<dyn assistant_runtime::ModelServiceFactory>>,
) -> bool {
    let OpenDomain {
        key,
        permit,
        slot,
        mut guard,
        reply,
    } = request;
    let access = match key.as_ref() {
        Some(key) => credentials.domain_access(key).map(|(access, _)| access),
        None => Ok(credentials.personal_transport()),
    };
    let access = match access {
        Ok(access) => access,
        Err(error) => {
            let _ = reply.send(Err(error.into()));
            return true;
        }
    };
    if permit.check().is_err() || shutdown.is_cancelled() {
        let _ = reply.send(Err(AccessError::Unauthorized.into()));
        return true;
    }
    let home = match &key {
        Some(key) => config
            .runtime_home
            .join("users")
            .join(format!("{}_{}", key.center_id, key.user_id)),
        None => crate::host_layout::personal_home(&config.runtime_home),
    };
    let source = Arc::new(LocalConfigSource::new(home.join("config.toml")));
    startup.restart();
    let mut resources = services::UserResources::new(
        access.child_token(),
        Arc::new(crate::user_paths::UserPaths::with_secrets(
            &home,
            shared.protected_files,
        )),
        shared.speech_slots,
    );
    if let Some(key) = &key {
        let domain = match credentials.domain_access(key) {
            Ok((_, domain)) => domain,
            Err(error) => {
                let _ = reply.send(Err(error.into()));
                return true;
            }
        };
        resources.models = Some(crate::resources::model::CenterModelLoader::new(
            credentials.clone(),
            key.clone(),
            Arc::downgrade(&domain),
            resources.cancellation.clone(),
        ));
    }
    let initialized = services::initialize(
        &home,
        config.event_capacity,
        key.is_none(),
        source,
        &startup,
        &mut resources,
        #[cfg(test)]
        model,
    )
    .await;
    let (services, gateway, speech) = match initialized {
        Ok(ready) => ready,
        Err(error) => {
            startup.fail(error);
            let cleaned = resources.shutdown().await;
            let _ = reply.send(Err(if cleaned {
                DomainError::Initialization(error)
            } else {
                DomainError::Shutdown
            }));
            if !cleaned {
                eprintln!(
                    "runtime-host: failed initialization resources retained; replacement is blocked"
                );
                shutdown.cancelled().await;
            }
            return cleaned;
        }
    };
    let (closed, completion) = watch::channel(None);
    let _completion = DomainCompletion {
        access: access.clone(),
        closed: closed.clone(),
    };
    *guard = Some(Arc::new(LoadedDomain {
        services: services.clone(),
        access: access.clone(),
        closed: completion,
    }));
    // 发布后仍检查访问；退出/Host 停止的晚到装配只能进入关闭，不能重新开放业务。
    let valid = !access.is_cancelled() && !shutdown.is_cancelled();
    let _ = reply.send(if valid {
        Ok(services.clone())
    } else {
        Err(DomainError::Shutdown)
    });
    drop(guard);
    let mut supervisor = HostSupervisor::new(Duration::from_secs(10));
    supervisor.spawn_subsystem("speech_service", FailurePolicy::Degrade, move |cancel| {
        speech.run_until(cancel)
    });
    supervisor.spawn_subsystem("device_gateway", FailurePolicy::Degrade, move |cancel| {
        gateway.run_until(cancel)
    });
    let runtime = services.runtime.clone();
    supervisor.spawn_subsystem("mcp_startup", FailurePolicy::Degrade, move |cancel| {
        crate::mcp_startup::run(runtime, cancel)
    });
    let observed = std::panic::AssertUnwindSafe(supervisor.run_until(async {
        monitor(&services, &credentials, key.as_ref(), &access, &shutdown).await;
        // 先停 Runtime 准入和执行，再等待设备及语音服务退出。
        access.cancel();
        services
            .runtime
            .request_shutdown()
            .map_err(std::io::Error::other)
    }))
    .catch_unwind()
    .await;
    access.cancel();
    let _ = services.runtime.request_shutdown();
    let mut guard = slot.lock().await;
    let stopped = resources.shutdown().await;
    let success = matches!(observed, Ok(Ok(()))) && stopped;
    if success {
        *guard = None;
    } else {
        eprintln!("runtime-host: user domain cleanup failed; replacement is blocked");
    }
    closed.send_replace(Some(success));
    success
}

async fn monitor(
    services: &ReadyServices,
    credentials: &Credentials,
    key: Option<&UserKey>,
    access: &CancellationToken,
    shutdown: &CancellationToken,
) {
    let mut changes = credentials.subscribe_changes();
    let mut work = services.runtime.subscribe_work_changes();
    let mut gateway = services.device_gateway.subscribe_events();
    loop {
        let active = services.runtime.has_background_work()
            || services
                .device_gateway
                .snapshot()
                .await
                .is_ok_and(|snapshot| snapshot.enabled);
        // 活动 Run 在最后一个客户端过期后仍参与既有 30s /me 复核。
        let _activity = if active {
            key.and_then(|key| {
                credentials
                    .domain_access(key)
                    .ok()
                    .map(|(_, activity)| activity)
            })
        } else {
            None
        };
        let deadline = credentials.login_deadline(key);
        if let Some(key) = key
            && !active
            && deadline.is_none()
            && services
                .runtime
                .request_shutdown_if_unused(|| credentials.end_unused_user(key))
                .await
                .unwrap_or(false)
        {
            break;
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = access.cancelled() => break,
            _ = changes.changed() => {},
            _ = work.changed() => {},
            _ = gateway.recv() => {},
            () = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => {},
        }
    }
}
