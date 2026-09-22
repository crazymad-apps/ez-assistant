//! 按明确用户根复用正式 Store、Runtime、MCP、Gateway 与 Speech 装配。
use crate::{
    config_source::LocalConfigSource,
    device::{DeviceChannelOutputDispatcher, DeviceGatewayService},
    http::{ReadyServices, StartupStateHandle},
    mcp::{HostMcpConnectionFactory, HostMcpImageMaterializer, LocalMcpConfigSource},
    recall_reference_key,
    resources::HostResources,
    speech::SpeechService,
    storage::LocalRuntimeStore,
};
use assistant_protocol::{
    ReloadConfigRequest, RuntimeHostStartupError, RuntimeHostStartupStage, ShutdownRuntimeRequest,
};
use assistant_runtime::{AssistantRuntime, RuntimeConfig, RuntimeStore};
use std::sync::Arc;

pub(super) type Initialized = (Arc<ReadyServices>, DeviceGatewayService, SpeechService);

/// 部分装配也有同一个具体 owner；失败关闭后才允许下一次开库。
pub(super) struct UserResources {
    paths: Arc<crate::user_paths::UserPaths>,
    speech_slots: crate::speech::SpeechSlots,
    pub(super) cancellation: tokio_util::sync::CancellationToken,
    store: Option<Arc<LocalRuntimeStore>>,
    runtime: Option<Arc<AssistantRuntime>>,
    mcp: Option<Arc<HostMcpConnectionFactory>>,
    pub(super) models: Option<Arc<crate::resources::model::CenterModelLoader>>,
    model_task: Option<tokio::task::JoinHandle<()>>,
}
impl UserResources {
    pub(super) fn new(
        cancellation: tokio_util::sync::CancellationToken,
        paths: Arc<crate::user_paths::UserPaths>,
        speech_slots: crate::speech::SpeechSlots,
    ) -> Self {
        Self {
            cancellation,
            paths,
            speech_slots,
            store: None,
            runtime: None,
            mcp: None,
            models: None,
            model_task: None,
        }
    }

    pub(super) async fn shutdown(&mut self) -> bool {
        self.cancellation.cancel();
        let models = if let Some(mut task) = self.model_task.take() {
            match tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await {
                Ok(result) => result.is_ok(),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    false
                }
            }
        } else {
            true
        };
        let runtime = if let Some(runtime) = &self.runtime {
            runtime
                .shutdown(ShutdownRuntimeRequest::default())
                .await
                .is_ok()
        } else if let Some(store) = &self.store {
            store.shutdown().await.is_ok()
        } else {
            true
        };
        let mcp = if let Some(mcp) = &self.mcp {
            mcp.shutdown().await.is_ok()
        } else {
            true
        };
        runtime && mcp && models
    }
}

pub(super) async fn initialize(
    user_home: &std::path::Path,
    event_capacity: std::num::NonZeroUsize,
    personal: bool,
    source: Arc<LocalConfigSource>,
    startup: &StartupStateHandle,
    owner: &mut UserResources,
    #[cfg(test)] model: Option<Arc<dyn assistant_runtime::ModelServiceFactory>>,
) -> Result<Initialized, RuntimeHostStartupError> {
    use RuntimeHostStartupError::*;
    let paths = owner.paths.clone();
    crate::config_source::prepare_private_directory(user_home).map_err(|_| InitializationFailed)?;
    startup.stage(RuntimeHostStartupStage::DatabaseCheck);
    let (progress, mut stages) =
        tokio::sync::watch::channel(crate::storage::DatabaseStartupProgress {
            stage: RuntimeHostStartupStage::DatabaseCheck,
            database_version: None,
            min_compatible_host_version: None,
        });
    let opening = LocalRuntimeStore::open_with_progress(user_home, 64, Some(progress));
    tokio::pin!(opening);
    let store = Arc::new(loop {
        tokio::select! {
            result = &mut opening => {
                // ready/error 通知可能先于 watch 分支被轮询；返回前读取最后一次可靠进度。
                startup.database_progress(stages.borrow_and_update().clone());
                break result.map_err(|error| crate::storage::migrations::startup_error(&error))?;
            },
            result = stages.changed() => {
                if result.is_ok() { startup.database_progress(stages.borrow_and_update().clone()); }
            }
        }
    });
    owner.store = Some(store.clone());
    // 数据库准入完成后才准备业务资源与 Recall 密钥；后续失败显式关闭已拥有的存储 worker。
    let prepared = (|| {
        let model_factory: Arc<dyn assistant_runtime::ModelServiceFactory> = match &owner.models {
            Some(loader) => Arc::new(crate::resources::model::CenterModelServiceFactory::new(
                user_home,
                loader.clone(),
            )),
            None if personal => Arc::new(crate::resources::model::HostModelServiceFactory::new(
                user_home,
            )),
            None => return Err(InitializationFailed),
        };
        let resources = HostResources::for_user(user_home, personal, model_factory, paths.clone())
            .map_err(|_| InitializationFailed)?;
        let key =
            recall_reference_key::load_or_create(user_home).map_err(|_| InitializationFailed)?;
        Ok::<_, RuntimeHostStartupError>((resources, key))
    })();
    let (resources, recall_key) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(error);
        }
    };
    #[cfg(test)]
    let resources = if let Some(model) = model {
        HostResources {
            model_factory: model,
            ..resources
        }
    } else {
        resources
    };
    startup.stage(RuntimeHostStartupStage::Configuration);
    if crate::storage::migrations::config_cleanup::cleanup(source.as_ref(), user_home)
        .await
        .is_err()
    {
        return Err(ConfigurationInvalid);
    }
    startup.stage(RuntimeHostStartupStage::Recovery);
    let dispatcher = Arc::new(DeviceChannelOutputDispatcher::new());
    let mcp_source = Arc::new(LocalMcpConfigSource::new(user_home.to_path_buf()));
    let mcp_factory = Arc::new(HostMcpConnectionFactory::new(user_home.to_path_buf()));
    owner.mcp = Some(mcp_factory.clone());
    let runtime = match AssistantRuntime::open_with_recall_key(
        RuntimeConfig::new(event_capacity),
        source.clone(),
        resources.model_factory,
        resources.session_environment_factory,
        resources.skill_package_source,
        resources.run_tool_factory,
        resources.child_task_workspace_factory,
        store.clone(),
        store.clone(),
        recall_key,
    )
    .await
    {
        Ok(runtime) => Arc::new(
            runtime
                .with_cancellation(owner.cancellation.clone())
                .with_mcp_services(
                    mcp_source,
                    mcp_factory.clone(),
                    Arc::new(HostMcpImageMaterializer),
                )
                .with_channel_output_dispatcher(dispatcher.clone()),
        ),
        Err(_) => {
            return Err(ConfigurationInvalid);
        }
    };
    owner.runtime = Some(runtime.clone());
    startup.stage(RuntimeHostStartupStage::Configuration);
    if runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .is_err()
    {
        return Err(ConfigurationInvalid);
    }
    if let Some(loader) = &owner.models {
        loader.attach(&runtime);
        owner.model_task = Some(tokio::spawn(loader.clone().run()));
    }
    let (speech_service, speech) = SpeechService::with_slots(source, &owner.speech_slots);
    let (gateway, gateway_handle) = DeviceGatewayService::new(
        user_home.to_path_buf(),
        runtime.clone(),
        dispatcher.as_ref(),
        speech.clone(),
    );
    startup.ready(
        paths,
        owner.cancellation.clone(),
        runtime.clone(),
        owner.models.clone(),
        gateway_handle,
        speech,
    );
    Ok((
        startup.services().map_err(|_| InitializationFailed)?,
        gateway,
        speech_service,
    ))
}
