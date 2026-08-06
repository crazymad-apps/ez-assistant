//! Agent SDK 与完整 Core 能力的独立 B/S 验证宿主。
//!
//! 该 binary 由开发者显式启动，不属于正式 Assistant Runtime 或桌面应用。

mod approval;
mod atomic_json;
mod audit;
mod cli;
mod compaction;
mod config;
mod journal;
mod memory;
#[cfg(test)]
mod model;
mod policy;
mod resources;
mod runtime;
mod server;
mod tooling;
mod wire;

use std::error::Error;

use cli::{CliAction, parse_cli};
use config::ServeConfig;
use runtime::DemoRuntime;
use server::DemoServer;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("core-demo: {error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    match parse_cli(std::env::args_os().skip(1))? {
        CliAction::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        CliAction::Serve(arguments) => {
            dotenvy::dotenv().ok();
            let config = ServeConfig::resolve(arguments)?;
            let runtime = DemoRuntime::new(config.clone()).await?;
            let server = DemoServer::bind(config.port, runtime).await?;

            println!("Core Demo is ready at {}", server.launch_url());
            println!("Workdir: {}", config.workdir.display());
            println!("Data dir: {}", config.data_dir.display());
            println!(
                "This is a temporary loopback-only validation host. Its in-memory sessions are not restored after exit."
            );
            println!(
                "The data directory is not isolated from Agent file or Shell tools; use dedicated validation data."
            );
            server.serve().await?;
            Ok(())
        }
    }
}
