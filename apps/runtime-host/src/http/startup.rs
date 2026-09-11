//! HTTP 监听唯一的启动状态；Ready 原子发布现有业务服务，初始化期间不构造占位 Runtime。

use crate::{device::DeviceGatewayHandle, speech::SpeechServiceHandle};
use assistant_protocol::{
    RuntimeHostHealth, RuntimeHostHealthStatus, RuntimeHostStartupError, RuntimeHostStartupStage,
};
use assistant_runtime::{AssistantRuntime, RuntimeError};
use std::sync::Arc;
use tokio::sync::watch;

pub(crate) struct ReadyServices {
    pub(super) runtime: Arc<AssistantRuntime>,
    pub(super) device_gateway: DeviceGatewayHandle,
    pub(super) speech: SpeechServiceHandle,
}

#[derive(Clone)]
enum StartupState {
    Starting(RuntimeHostHealth),
    Ready(Arc<ReadyServices>, RuntimeHostHealth),
    Unavailable(RuntimeHostHealth),
}

/// watch 只保留当前状态；克隆句柄不是第二份业务状态，不持锁跨异步操作。
#[derive(Clone)]
pub(crate) struct StartupStateHandle(watch::Sender<StartupState>);

impl StartupStateHandle {
    pub(crate) fn new() -> Self {
        Self(
            watch::channel(StartupState::Starting(health(
                RuntimeHostHealthStatus::Starting,
                Some(RuntimeHostStartupStage::DatabaseCheck),
                None,
            )))
            .0,
        )
    }
    pub(crate) fn stage(&self, stage: RuntimeHostStartupStage) {
        self.0.send_if_modified(|state| {
            if let StartupState::Starting(current) = state {
                current.stage = Some(stage);
                true
            } else {
                false
            }
        });
    }
    /// 只投影存储线程已经核验或提交的版本；后续阶段失败仍保留该事实。
    pub(crate) fn database_progress(&self, progress: crate::storage::DatabaseStartupProgress) {
        self.0.send_if_modified(|state| {
            if let StartupState::Starting(current) = state {
                current.stage = Some(progress.stage);
                current.database_version = progress.database_version.clone();
                current.min_compatible_host_version = progress.min_compatible_host_version.clone();
                true
            } else {
                false
            }
        });
    }
    pub(crate) fn fail(&self, error: RuntimeHostStartupError) {
        self.0.send_if_modified(|state| {
            if let StartupState::Starting(current) = state {
                let mut failed = current.clone();
                failed.status = RuntimeHostHealthStatus::Unavailable;
                failed.error = Some(error);
                *state = StartupState::Unavailable(failed);
                true
            } else {
                false
            }
        });
    }
    pub(crate) fn ready(
        &self,
        runtime: Arc<AssistantRuntime>,
        device_gateway: DeviceGatewayHandle,
        speech: SpeechServiceHandle,
    ) {
        self.0.send_if_modified(|state| {
            if let StartupState::Starting(current) = state {
                let mut ready = current.clone();
                ready.status = RuntimeHostHealthStatus::Ready;
                ready.stage = None;
                ready.error = None;
                *state = StartupState::Ready(
                    Arc::new(ReadyServices {
                        runtime: runtime.clone(),
                        device_gateway: device_gateway.clone(),
                        speech: speech.clone(),
                    }),
                    ready,
                );
                true
            } else {
                false
            }
        });
    }
    pub(super) fn services(&self) -> Result<Arc<ReadyServices>, RuntimeError> {
        match &*self.0.borrow() {
            StartupState::Ready(services, _) => Ok(services.clone()),
            _ => Err(RuntimeError::StorageUnavailable {
                operation: "access Host before initialization",
                source: None,
            }),
        }
    }
    pub(crate) fn health(&self) -> RuntimeHostHealth {
        match &*self.0.borrow() {
            StartupState::Starting(current) | StartupState::Unavailable(current) => current.clone(),
            StartupState::Ready(_, current) => current.clone(),
        }
    }
}
fn health(
    status: RuntimeHostHealthStatus,
    stage: Option<RuntimeHostStartupStage>,
    error: Option<RuntimeHostStartupError>,
) -> RuntimeHostHealth {
    RuntimeHostHealth {
        status,
        stage,
        error,
        database_version: (status == RuntimeHostHealthStatus::Ready)
            .then(|| env!("CARGO_PKG_VERSION").into()),
        target_version: env!("CARGO_PKG_VERSION").into(),
        min_compatible_host_version: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_failure_retains_the_committed_database_version() {
        let startup = StartupStateHandle::new();
        startup.database_progress(crate::storage::DatabaseStartupProgress {
            stage: RuntimeHostStartupStage::Recovery,
            database_version: Some("0.25.1".into()),
            min_compatible_host_version: None,
        });
        startup.stage(RuntimeHostStartupStage::Configuration);
        startup.fail(RuntimeHostStartupError::ConfigurationInvalid);
        let health = startup.health();
        assert_eq!(health.status, RuntimeHostHealthStatus::Unavailable);
        assert_eq!(health.stage, Some(RuntimeHostStartupStage::Configuration));
        assert_eq!(health.database_version.as_deref(), Some("0.25.1"));
        assert!(startup.services().is_err());
    }
}
