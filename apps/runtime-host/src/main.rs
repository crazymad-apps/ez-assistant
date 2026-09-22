//! EZ Assistant 正式 Runtime Host 进程入口。

mod access;
mod attachment_hash;
mod config;
mod config_source;
mod device;
mod endpoint;
mod host_configuration;
mod host_layout;
mod http;
mod image;
mod mcp;
mod mcp_startup;
mod media_diagnostics;
mod platform;
mod recall_reference_key;
mod resources;
mod server;
mod speech;
mod storage;
mod supervisor;
mod user_domain;
mod user_paths;
mod user_terminal;

mod startup;

use crate::config::{CliAction, parse_cli};
use crate::{
    config::{LaunchConfig, ServeConfig},
    config_source::prepare_runtime_home,
};
use std::error::Error;

fn main() {
    let action = match parse_cli(std::env::args_os().skip(1)) {
        Ok(action) => action,
        Err(error) => error.exit(),
    };
    if matches!(&action, CliAction::Serve(arguments) if arguments.detached)
        && let Err(error) = platform::detach_session()
    {
        eprintln!("runtime-host: cannot detach session: {error}");
        std::process::exit(2);
    }
    if matches!(action, CliAction::BuildInfoJson) {
        println!(
            "{}",
            serde_json::to_string(&assistant_protocol::ClientCompatibility::current())
                .expect("build info")
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Host async runtime");
    if let Err(error) = runtime.block_on(run(action)) {
        eprintln!("runtime-host: {error}");
        std::process::exit(2);
    }
}

async fn run(action: CliAction) -> Result<(), Box<dyn Error>> {
    match action {
        CliAction::BuildInfoJson => {
            println!(
                "{}",
                serde_json::to_string(&assistant_protocol::ClientCompatibility::current())?
            );
            Ok(())
        }
        CliAction::Access(arguments) => access::offline::run(arguments).await,
        CliAction::Launch(arguments) => {
            let config = LaunchConfig::resolve(arguments)?;
            prepare_runtime_home(&config.runtime_home)?;
            platform::launch_detached(&config.runtime_home)?;
            Ok(())
        }
        CliAction::Serve(arguments) => startup::serve(ServeConfig::resolve(arguments)?).await,
    }
}
