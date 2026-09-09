//! EZ Assistant 正式 Runtime Host 进程入口。

#[cfg(unix)]
mod access;
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
#[cfg(unix)]
mod mcp_startup;
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
mod user_terminal;

#[cfg(unix)]
mod startup;

use crate::config::{CliAction, parse_cli};
#[cfg(unix)]
use crate::{
    config::{LaunchConfig, ServeConfig},
    config_source::prepare_runtime_home,
};
use std::error::Error;
#[cfg(not(unix))]
use thiserror::Error;

#[cfg(unix)]
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
                startup::serve(ServeConfig::resolve(arguments)?).await
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
