//! 认证与监听先于业务初始化；失败保留诊断监听，业务服务和监督任务只在真实初始化成功后发布。

use crate::{
    access,
    config::ServeConfig,
    config_source::{LocalConfigSource, prepare_runtime_home},
    device::{DeviceChannelOutputDispatcher, DeviceGatewayService},
    endpoint::RuntimeInstanceGuard,
    http::{HttpEndpointState, HttpState, StartupStateHandle},
    mcp::{HostMcpConnectionFactory, HostMcpImageMaterializer, LocalMcpConfigSource},
    mcp_startup, recall_reference_key,
    resources::HostResources,
    server::RuntimeServer,
    speech::SpeechService,
    storage::LocalRuntimeStore,
    supervisor::{FailurePolicy, HostSupervisor},
    user_terminal,
};
use assistant_protocol::{
    ReloadConfigRequest, RuntimeHostStartupError, RuntimeHostStartupStage, ShutdownRuntimeRequest,
};
use assistant_runtime::{AssistantRuntime, RuntimeConfig, RuntimeStore};
use std::{error::Error, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) async fn serve(config: ServeConfig) -> Result<(), Box<dyn Error>> {
    prepare_runtime_home(&config.runtime_home)?;
    let instance = RuntimeInstanceGuard::acquire(&config.runtime_home)?;
    let config_source = Arc::new(LocalConfigSource::new(config.config_path.clone()));
    let (mut access_service, access_handle) = access::HostAccessService::new(config_source.clone());
    if config.password_stdin {
        use tokio::io::AsyncReadExt as _;
        let mut bytes = Vec::new();
        tokio::io::stdin()
            .take(1027)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        let password = String::from_utf8(bytes)
            .map_err(|_| access::AccessError::Invalid("密码必须为 UTF-8。"))?;
        access_service
            .initialize_password(assistant_protocol::SecretValue::new(password))
            .await?;
    }
    // 认证/TLS/绑定失败仍走监听前错误；绝不为展示诊断放松认证或另开端口。
    let configuration = access_service.prepare().await?;
    let endpoint = instance.bind_and_publish(&configuration).await?;
    let mut supervisor = HostSupervisor::new(SHUTDOWN_TIMEOUT);
    let shutdown = supervisor.shutdown_handle();
    let terminals = user_terminal::UserTerminalService::new();
    let state = HttpState::starting(
        HttpEndpointState::new(
            endpoint.access_token(),
            endpoint.authority(),
            endpoint.base_url(),
            config.runtime_home.clone(),
            endpoint.instance_id().into(),
        ),
        shutdown,
        access_handle,
        terminals.handle.clone(),
    );
    println!(
        "EZ Assistant Runtime is listening at {} (discovery: {})",
        endpoint.base_url(),
        endpoint.discovery_path().display()
    );
    let server = RuntimeServer::new(endpoint, state.clone());
    supervisor.spawn_subsystem("host_http", FailurePolicy::ShutdownHost, move |shutdown| {
        server.serve_until(shutdown)
    });
    let access_state = state.clone();
    supervisor.spawn_subsystem("host_access", FailurePolicy::Degrade, move |shutdown| {
        access_service.run_until(access_state, shutdown)
    });
    supervisor.spawn_subsystem("user_terminals", FailurePolicy::Degrade, move |shutdown| {
        terminals.run_until(shutdown)
    });
    supervisor.spawn_subsystem(
        "runtime_services",
        FailurePolicy::ShutdownHost,
        move |shutdown| run_services(config, config_source, state.startup, shutdown),
    );
    supervisor
        .run_until(async { tokio::signal::ctrl_c().await })
        .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("Runtime initialization or shutdown failed")]
struct StartupFailure;

/// 初始化由 Host Supervisor 所有，不随 HTTP 请求或 WebView 生命周期取消。
/// 失败后不循环重试 SQL；保持诊断直到 Host 被显式停止。所有已创建服务均由本任务收尾。
async fn run_services(
    config: ServeConfig,
    source: Arc<LocalConfigSource>,
    startup: StartupStateHandle,
    shutdown: CancellationToken,
) -> Result<(), StartupFailure> {
    let initialized = initialize(&config, source, &startup).await;
    let (runtime, mcp_factory, device_gateway, speech_service) = match initialized {
        Ok(ready) => ready,
        Err(code) => {
            startup.fail(code);
            eprintln!("runtime-host: startup unavailable: {code:?}");
            shutdown.cancelled().await;
            return Ok(());
        }
    };
    let mut supervisor = HostSupervisor::new(SHUTDOWN_TIMEOUT);
    supervisor.spawn_subsystem("speech_service", FailurePolicy::Degrade, move |shutdown| {
        speech_service.run_until(shutdown)
    });
    supervisor.spawn_subsystem("device_gateway", FailurePolicy::Degrade, move |shutdown| {
        device_gateway.run_until(shutdown)
    });
    let mcp_runtime = runtime.clone();
    supervisor.spawn_subsystem("mcp_startup", FailurePolicy::Degrade, move |shutdown| {
        mcp_startup::run(mcp_runtime, shutdown)
    });
    let result = supervisor
        .run_until(async {
            shutdown.cancelled().await;
            Ok::<(), std::io::Error>(())
        })
        .await;
    let stopped = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
    let mcp_stopped = mcp_factory.shutdown().await;
    if result.is_err() || stopped.is_err() || mcp_stopped.is_err() {
        return Err(StartupFailure);
    }
    Ok(())
}

type Initialized = (
    Arc<AssistantRuntime>,
    Arc<HostMcpConnectionFactory>,
    DeviceGatewayService,
    SpeechService,
);

async fn initialize(
    config: &ServeConfig,
    source: Arc<LocalConfigSource>,
    startup: &StartupStateHandle,
) -> Result<Initialized, RuntimeHostStartupError> {
    use RuntimeHostStartupError::*;
    let resources = HostResources::new(&config.runtime_home).map_err(|_| InitializationFailed)?;
    let recall_key = recall_reference_key::load_or_create(&config.runtime_home)
        .map_err(|_| InitializationFailed)?;
    startup.stage(RuntimeHostStartupStage::DatabaseCheck);
    let (progress, mut stages) =
        tokio::sync::watch::channel(crate::storage::DatabaseStartupProgress {
            stage: RuntimeHostStartupStage::DatabaseCheck,
            database_version: None,
        });
    let opening = LocalRuntimeStore::open_with_progress(&config.runtime_home, 64, Some(progress));
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
    startup.stage(RuntimeHostStartupStage::Configuration);
    if crate::storage::migrations::config_cleanup::cleanup(source.as_ref(), &config.runtime_home)
        .await
        .is_err()
    {
        let _ = store.shutdown().await;
        return Err(ConfigurationInvalid);
    }
    startup.stage(RuntimeHostStartupStage::Recovery);
    let dispatcher = Arc::new(DeviceChannelOutputDispatcher::new());
    let mcp_source = Arc::new(LocalMcpConfigSource::new(config.runtime_home.clone()));
    let mcp_factory = Arc::new(HostMcpConnectionFactory::new(config.runtime_home.clone()));
    let runtime = match AssistantRuntime::open_with_recall_key(
        RuntimeConfig::new(config.event_capacity),
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
                .with_mcp_services(
                    mcp_source,
                    mcp_factory.clone(),
                    Arc::new(HostMcpImageMaterializer),
                )
                .with_channel_output_dispatcher(dispatcher.clone()),
        ),
        Err(_) => {
            let _ = store.shutdown().await;
            return Err(ConfigurationInvalid);
        }
    };
    startup.stage(RuntimeHostStartupStage::Configuration);
    if runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .is_err()
    {
        let _ = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
        let _ = mcp_factory.shutdown().await;
        return Err(ConfigurationInvalid);
    }
    let (speech_service, speech) = SpeechService::new(source);
    let (gateway, gateway_handle) = DeviceGatewayService::new(
        config.runtime_home.clone(),
        runtime.clone(),
        dispatcher.as_ref(),
        speech.clone(),
    );
    startup.ready(runtime.clone(), gateway_handle, speech);
    Ok((runtime, mcp_factory, gateway, speech_service))
}
