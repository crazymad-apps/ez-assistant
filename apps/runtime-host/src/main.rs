//! EZ Assistant 正式 Runtime Host 进程入口。

#[cfg(unix)]
mod attachment_hash;
mod config;
#[cfg(unix)]
mod config_source;
#[cfg(unix)]
mod device;
#[cfg(unix)]
mod endpoint;
#[cfg(unix)]
mod http;
mod image;
#[cfg(unix)]
mod mcp;
mod media_diagnostics;
#[cfg(unix)]
mod platform;
#[cfg(unix)]
mod recall_reference_key;
mod resources;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod speech;
#[cfg(unix)]
mod storage;
#[cfg(unix)]
mod supervisor;

use std::error::Error;
#[cfg(unix)]
use std::{sync::Arc, time::Duration};

#[cfg(unix)]
use assistant_protocol::{ReloadConfigRequest, ShutdownRuntimeRequest};
#[cfg(unix)]
use assistant_runtime::{AssistantRuntime, ModelCatalog, RuntimeConfig, RuntimeStore};
#[cfg(not(unix))]
use thiserror::Error;

use crate::config::{CliAction, parse_cli};
#[cfg(unix)]
use crate::{
    config::{LaunchConfig, ServeConfig},
    config_source::{LocalConfigSource, prepare_runtime_home},
    device::{DeviceChannelOutputDispatcher, DeviceGatewayService},
    endpoint::RuntimeInstanceGuard,
    mcp::{HostMcpConnectionFactory, HostMcpImageMaterializer, LocalMcpConfigSource},
    resources::HostResources,
    server::RuntimeServer,
    speech::SpeechService,
    storage::LocalRuntimeStore,
    supervisor::{FailurePolicy, HostSupervisor, SupervisorError},
};

#[cfg(unix)]
const STORAGE_QUEUE_CAPACITY: usize = 64;
#[cfg(unix)]
const MODEL_CATALOG_JSON: &str = include_str!("../resources/model-catalog.json");
#[cfg(unix)]
const HOST_SUBSYSTEM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::main]
async fn main() {
    let action = match parse_cli(std::env::args_os().skip(1)) {
        Ok(action) => action,
        Err(error) => error.exit(),
    };
    if let Err(error) = run(action).await {
        eprintln!("runtime-host: {error}");
        std::process::exit(2);
    }
}

async fn run(action: CliAction) -> Result<(), Box<dyn Error>> {
    match action {
        CliAction::Launch(arguments) => {
            #[cfg(unix)]
            {
                let config = LaunchConfig::resolve(arguments)?;
                prepare_runtime_home(&config.runtime_home)?;
                platform::launch_detached(&config.runtime_home)?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = arguments;
                Err(Box::new(UnsupportedPlatform))
            }
        }
        CliAction::Serve(arguments) => {
            #[cfg(unix)]
            {
                let config = ServeConfig::resolve(arguments)?;
                prepare_runtime_home(&config.runtime_home)?;
                // 单实例锁必须先于 Store；HTTP 端口与发现文件在 Runtime 恢复后才发布。
                let instance = RuntimeInstanceGuard::acquire(&config.runtime_home)?;
                let resources = HostResources::new(&config.runtime_home)?;
                let model_catalog = Arc::new(ModelCatalog::from_json(MODEL_CATALOG_JSON)?);
                let recall_reference_key =
                    recall_reference_key::load_or_create(&config.runtime_home)?;
                let store = Arc::new(
                    LocalRuntimeStore::open(&config.runtime_home, STORAGE_QUEUE_CAPACITY).await?,
                );
                let device_output_dispatcher = Arc::new(DeviceChannelOutputDispatcher::new());
                let config_source = Arc::new(LocalConfigSource::new(config.config_path.clone()));
                let mcp_config_source =
                    Arc::new(LocalMcpConfigSource::new(config.runtime_home.clone()));
                let mcp_connection_factory =
                    Arc::new(HostMcpConnectionFactory::new(config.runtime_home.clone()));
                let runtime = match AssistantRuntime::open_with_recall_key(
                    RuntimeConfig::new(config.event_capacity),
                    config_source.clone(),
                    model_catalog,
                    resources.model_factory,
                    resources.session_environment_factory,
                    resources.skill_package_source,
                    resources.run_tool_factory,
                    resources.child_task_workspace_factory,
                    store.clone(),
                    store.clone(),
                    recall_reference_key,
                )
                .await
                {
                    Ok(runtime) => Arc::new(
                        runtime
                            .with_mcp_services(
                                mcp_config_source,
                                mcp_connection_factory.clone(),
                                Arc::new(HostMcpImageMaterializer),
                            )
                            .with_channel_output_dispatcher(device_output_dispatcher.clone()),
                    ),
                    Err(error) => {
                        store.shutdown().await?;
                        return Err(Box::new(error));
                    }
                };
                if let Err(error) = runtime.reload_config(ReloadConfigRequest::default()).await {
                    let _ = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
                    if mcp_connection_factory.shutdown().await.is_err() {
                        eprintln!("runtime-host: MCP cleanup failed during startup rollback");
                    }
                    return Err(Box::new(error));
                }
                // MCP 是可降级的外部能力。bootstrap 必须在监听端点发布前完成，但配置或
                // Server 故障不能阻止其余 Runtime 能力启动。
                if runtime.bootstrap_mcp().await.is_err() {
                    eprintln!("runtime-host: MCP bootstrap is unavailable");
                }
                let endpoint = match instance.bind_and_publish().await {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        let _ = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
                        if mcp_connection_factory.shutdown().await.is_err() {
                            eprintln!("runtime-host: MCP cleanup failed during endpoint rollback");
                        }
                        return Err(Box::new(error));
                    }
                };
                println!(
                    "EZ Assistant Runtime is listening at {} (discovery: {})",
                    endpoint.base_url(),
                    endpoint.discovery_path().display()
                );
                let (speech_service, speech_handle) = SpeechService::new(config_source);
                let (device_gateway, device_gateway_handle) = DeviceGatewayService::new(
                    config.runtime_home.clone(),
                    runtime.clone(),
                    device_output_dispatcher.as_ref(),
                    speech_handle.clone(),
                );
                let server = RuntimeServer::new(
                    endpoint,
                    runtime.clone(),
                    config.runtime_home,
                    device_gateway_handle,
                    speech_handle,
                );
                let mut supervisor = HostSupervisor::new(HOST_SUBSYSTEM_SHUTDOWN_TIMEOUT);
                let host_shutdown = supervisor.shutdown_handle();
                let command_shutdown = host_shutdown.clone();
                supervisor.spawn_subsystem(
                    "desktop_http",
                    FailurePolicy::ShutdownHost,
                    move |subsystem_shutdown| {
                        server.serve_until(subsystem_shutdown, command_shutdown)
                    },
                );
                supervisor.spawn_subsystem(
                    "speech_service",
                    FailurePolicy::Degrade,
                    move |subsystem_shutdown| speech_service.run_until(subsystem_shutdown),
                );
                supervisor.spawn_subsystem(
                    "device_gateway",
                    FailurePolicy::Degrade,
                    move |subsystem_shutdown| device_gateway.run_until(subsystem_shutdown),
                );

                let supervisor_result = supervisor
                    .run_until(async { tokio::signal::ctrl_c().await })
                    .await;
                // Host 入口已经停止或被 deadline 强制回收；无论 Supervisor 是否报错，都必须
                // 让 Runtime 结算活动 Run 并 flush/join Store，不能因主入口故障遗留 worker。
                let runtime_result = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
                mcp_connection_factory.shutdown().await?;
                match (supervisor_result, runtime_result) {
                    (Ok(()), Ok(_)) => Ok(()),
                    (Err(supervisor), Ok(_)) => Err(Box::new(supervisor) as Box<dyn Error>),
                    (Ok(()), Err(runtime)) => Err(Box::new(runtime) as Box<dyn Error>),
                    (Err(supervisor), Err(runtime)) => Err(Box::new(HostShutdownError {
                        supervisor,
                        runtime,
                    })
                        as Box<dyn Error>),
                }
            }
            #[cfg(not(unix))]
            {
                let _ = arguments;
                Err(Box::new(UnsupportedPlatform))
            }
        }
    }
}

#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[error("Host supervisor failed ({supervisor}); Runtime shutdown also failed ({runtime})")]
struct HostShutdownError {
    supervisor: SupervisorError,
    runtime: assistant_runtime::RuntimeError,
}

#[cfg(not(unix))]
#[derive(Debug, Error)]
#[error(
    "this Runtime Host build does not support the current platform; the current local storage adapter requires Unix filesystem semantics"
)]
struct UnsupportedPlatform;
