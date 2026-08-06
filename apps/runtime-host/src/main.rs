//! EZ Assistant 正式 Runtime Host 进程入口。

mod config;
#[cfg(all(feature = "demo-client", unix))]
mod demo;
#[cfg(unix)]
mod endpoint;
mod resources;
#[cfg(unix)]
mod server;
mod wire;

use std::error::Error;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use assistant_runtime::{AssistantRuntime, RuntimeConfig};
#[cfg(not(unix))]
use thiserror::Error;

use crate::config::{CliAction, parse_cli};
#[cfg(unix)]
use crate::{
    config::ServeConfig, endpoint::OwnedEndpoint, resources::HostResources, server::RuntimeServer,
};

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
                dotenvy::dotenv().ok();
                let config = ServeConfig::resolve(arguments)?;
                let resources = HostResources::from_config(&config)?;
                let runtime = Arc::new(AssistantRuntime::new(
                    RuntimeConfig::new(config.event_capacity),
                    resources.factory,
                    resources.default_authorizer,
                ));
                let endpoint = OwnedEndpoint::bind(config.socket_path.clone())?;
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
