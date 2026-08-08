//! EZ Assistant 正式 Runtime Host 进程入口。

mod config;
#[cfg(unix)]
mod config_source;
#[cfg(all(feature = "demo-client", unix))]
mod demo;
#[cfg(unix)]
mod endpoint;
mod resources;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod storage;
mod wire;

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
    config::ServeConfig,
    config_source::{LocalConfigSource, prepare_runtime_home},
    endpoint::OwnedEndpoint,
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
        CliAction::Serve(arguments) => {
            #[cfg(unix)]
            {
                let config = ServeConfig::resolve(arguments)?;
                prepare_runtime_home(&config.runtime_home)?;
                let endpoint = OwnedEndpoint::bind(config.socket_path.clone())?;
                let resources = HostResources::new()?;
                let store = Arc::new(
                    LocalRuntimeStore::open(&config.runtime_home, STORAGE_QUEUE_CAPACITY).await?,
                );
                let runtime = match AssistantRuntime::open(
                    RuntimeConfig::new(config.event_capacity),
                    Arc::new(LocalConfigSource::new(config.config_path)),
                    resources.model_factory,
                    resources.system_prompt_factory,
                    resources.tools,
                    resources.default_authorizer,
                    store.clone(),
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
                println!(
                    "EZ Assistant Runtime is listening at {}",
                    endpoint.path().display()
                );
                RuntimeServer::new(endpoint, runtime).serve().await?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = arguments;
                Err(Box::new(UnsupportedPlatform))
            }
        }
        #[cfg(feature = "demo-client")]
        CliAction::Demo(arguments) => {
            #[cfg(unix)]
            {
                let config = config::DemoConfig::resolve(arguments)?;
                demo::run(config).await?;
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
    "this Runtime Host build does not support the current platform; Unix domain sockets are required in v0.8.0"
)]
struct UnsupportedPlatform;
