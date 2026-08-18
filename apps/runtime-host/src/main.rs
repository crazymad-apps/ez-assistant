//! EZ Assistant 正式 Runtime Host 进程入口。

#[cfg(unix)]
mod attachment_hash;
mod config;
#[cfg(unix)]
mod config_source;
#[cfg(unix)]
mod endpoint;
#[cfg(unix)]
mod http;
#[cfg(unix)]
mod platform;
#[cfg(unix)]
mod recall_reference_key;
mod resources;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod storage;

use std::error::Error;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use assistant_protocol::{ReloadConfigRequest, ShutdownRuntimeRequest};
#[cfg(unix)]
use assistant_runtime::{AssistantRuntime, RuntimeConfig, RuntimeStore};
#[cfg(not(unix))]
use thiserror::Error;

use crate::config::{CliAction, parse_cli};
#[cfg(unix)]
use crate::{
    config::{LaunchConfig, ServeConfig},
    config_source::{LocalConfigSource, prepare_runtime_home},
    endpoint::RuntimeInstanceGuard,
    resources::HostResources,
    server::RuntimeServer,
    storage::LocalRuntimeStore,
};

#[cfg(unix)]
const STORAGE_QUEUE_CAPACITY: usize = 64;

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
                let recall_reference_key =
                    recall_reference_key::load_or_create(&config.runtime_home)?;
                let store = Arc::new(
                    LocalRuntimeStore::open(&config.runtime_home, STORAGE_QUEUE_CAPACITY).await?,
                );
                let runtime = match AssistantRuntime::open_with_recall_key(
                    RuntimeConfig::new(config.event_capacity),
                    Arc::new(LocalConfigSource::new(config.config_path)),
                    resources.model_factory,
                    resources.session_environment_factory,
                    resources.run_tool_factory,
                    resources.child_task_workspace_factory,
                    store.clone(),
                    store.clone(),
                    recall_reference_key,
                )
                .await
                {
                    Ok(runtime) => Arc::new(runtime),
                    Err(error) => {
                        store.shutdown().await?;
                        return Err(Box::new(error));
                    }
                };
                if let Err(error) = runtime.reload_config(ReloadConfigRequest::default()).await {
                    let _ = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
                    return Err(Box::new(error));
                }
                let endpoint = match instance.bind_and_publish().await {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        let _ = runtime.shutdown(ShutdownRuntimeRequest::default()).await;
                        return Err(Box::new(error));
                    }
                };
                println!(
                    "EZ Assistant Runtime is listening at {} (discovery: {})",
                    endpoint.base_url(),
                    endpoint.discovery_path().display()
                );
                RuntimeServer::new(endpoint, runtime, config.runtime_home)
                    .serve()
                    .await?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = arguments;
                Err(Box::new(UnsupportedPlatform))
            }
        }
    }
}

#[cfg(not(unix))]
#[derive(Debug, Error)]
#[error(
    "this Runtime Host build does not support the current platform; the current local storage adapter requires Unix filesystem semantics"
)]
struct UnsupportedPlatform;
