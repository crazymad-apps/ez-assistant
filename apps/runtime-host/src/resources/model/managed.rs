//! 用户域拥有唯一加载任务；Runtime 持有效配置，加载器只协调在途刷新和当前凭据。

mod transport;

use std::{
    path::Path,
    sync::{Arc, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use assistant_runtime::{
    AssistantRuntime, ExternalModelConfiguration, ModelServiceFactory, ModelServiceFactoryError,
    ModelServiceFactoryRequest, ModelSource, RuntimeError, RuntimeResult,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::HostModelServiceFactory;
use crate::access::{
    Credentials,
    enterprise::{CenterError, CenterLogin, UserKey},
};

pub(crate) struct CenterModelLoader {
    credentials: Arc<Credentials>,
    key: UserKey,
    domain: Weak<()>,
    cancellation: CancellationToken,
    runtime: OnceLock<Weak<AssistantRuntime>>,
    requests: watch::Sender<()>,
    completions: watch::Sender<Option<Result<(), CenterError>>>,
}

impl CenterModelLoader {
    pub(crate) fn new(
        credentials: Arc<Credentials>,
        key: UserKey,
        domain: Weak<()>,
        cancellation: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            credentials,
            key,
            domain,
            cancellation,
            runtime: OnceLock::new(),
            requests: watch::channel(()).0,
            completions: watch::channel(None).0,
        })
    }

    pub(crate) fn attach(&self, runtime: &Arc<AssistantRuntime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }

    fn runtime(&self) -> RuntimeResult<Arc<AssistantRuntime>> {
        self.runtime
            .get()
            .and_then(Weak::upgrade)
            .ok_or(RuntimeError::ConfigurationUnavailable)
    }

    fn login(&self) -> Result<(Arc<()>, Arc<CenterLogin>), CenterError> {
        let domain = self
            .domain
            .upgrade()
            .ok_or(CenterError::InvalidCredentials)?;
        let login = self.credentials.model_login(&self.key, &domain)?;
        Ok((domain, login))
    }

    /// 每个页面只等待共享结果，取消等待者不取消用户域拥有的网络任务。
    pub(crate) async fn refresh(&self) -> RuntimeResult<()> {
        let mut completion = self.completions.subscribe();
        self.requests.send_replace(());
        tokio::select! {
            () = self.cancellation.cancelled() => Err(RuntimeError::ConfigurationUnavailable),
            result = completion.changed() => {
                result.map_err(|_| RuntimeError::ConfigurationUnavailable)?;
                completion.borrow().as_ref().copied().unwrap_or(Err(CenterError::Unavailable))
                    .map_err(|_| RuntimeError::ConfigurationUnavailable)
            }
        }
    }

    /// 首次装配即加载；watch 合并并发意图，旧登录结果不发布，最新登录在下一轮获取。
    /// 用户资源 owner 持 JoinHandle，关闭时取消并等待；不派生脱离 owner 的任务。
    pub(crate) async fn run(self: Arc<Self>) {
        let mut requests = self.requests.subscribe();
        let mut identities = self.credentials.subscribe_changes();
        loop {
            requests.borrow_and_update();
            identities.borrow_and_update();
            let Ok((_domain, login)) = self.login() else {
                return;
            };
            let result = tokio::select! {
                () = self.cancellation.cancelled() => return,
                result = login.model_configuration(&self.key) => result,
            };
            let completion = result.as_ref().map(|_| ()).map_err(|error| *error);
            let Ok(runtime) = self.runtime() else {
                return;
            };
            let gate = tokio::select! {
                () = self.cancellation.cancelled() => return,
                gate = runtime.external_model_publication() => gate,
            };
            let Ok(mut gate) = gate else {
                return;
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|value| i64::try_from(value.as_millis()).ok())
                .unwrap_or(0);
            let published = self
                .credentials
                .publish_model_configuration(&self.key, &_domain, &login, &mut gate, result, now);
            gate.finish();
            requests.borrow_and_update();
            match published {
                Ok(true) => {
                    self.completions.send_replace(Some(completion));
                }
                Ok(false) => continue,
                Err(_) => {
                    self.completions
                        .send_replace(Some(Err(CenterError::Unavailable)));
                }
            }
            // 同一登录下请求在途时已被本次加载满足；新登录不能被合并掉。
            loop {
                tokio::select! {
                    () = self.cancellation.cancelled() => return,
                    _ = requests.changed() => break,
                    _ = identities.changed() => {
                        match self.login() {
                            Ok((_, current)) if Arc::ptr_eq(&current, &login) => {},
                            Ok(_) => break,
                            Err(_) => return,
                        }
                    }
                }
            }
        }
    }

    async fn rejected_configuration(
        &self,
        captured: &Arc<ExternalModelConfiguration>,
    ) -> RuntimeResult<()> {
        let runtime = self.runtime()?;
        let mut gate = runtime.external_model_publication().await?;
        let invalidated = gate.invalidate(captured, "中心模型配置已变化，正在刷新。")?;
        gate.finish();
        if invalidated {
            self.refresh().await?;
        }
        Ok(())
    }
}

pub(crate) struct CenterModelServiceFactory {
    loader: Arc<CenterModelLoader>,
    local: HostModelServiceFactory,
}

impl CenterModelServiceFactory {
    pub(crate) fn new(home: &Path, loader: Arc<CenterModelLoader>) -> Self {
        Self {
            loader,
            local: HostModelServiceFactory::new(home),
        }
    }
}

impl ModelServiceFactory for CenterModelServiceFactory {
    fn configuration_source(&self) -> ModelSource {
        ModelSource::External
    }

    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<agent_model::ModelServiceBundle, ModelServiceFactoryError> {
        let captured = request
            .external_configuration
            .ok_or_else(|| {
                ModelServiceFactoryError::new("external model configuration is required")
            })?
            .clone();
        let transport =
            transport::CenterModelTransport::new(self.loader.clone(), captured, &request)?;
        self.local
            .create_with_transport(request, Some(Arc::new(transport)))
    }
}
