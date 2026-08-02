//! 安全策略与真实工具执行的独立开发宿主。
//!
//! 该 binary 需由开发者显式启动，不是产品 Assistant Runtime 或桌面应用。

mod approval;
mod audit;
mod cli;
mod config;
mod journal;
mod policy;
mod resources;
mod runtime;
mod server;
mod wire;

use thiserror::Error;

use crate::{
    cli::CliAction,
    resources::{DemoResources, ResourceError},
    runtime::{DemoRuntime, WorkspaceError},
    server::DemoServer,
};

#[tokio::main]
async fn main() {
    match run().await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("safety-demo: {error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<(), AppError> {
    let config = match cli::parse()? {
        CliAction::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        CliAction::Run(config) => config,
    };
    dotenvy::dotenv().ok();
    let resources = DemoResources::from_environment(config.session_workdir())?;
    let runtime =
        DemoRuntime::new_with_resources(config.session_workdir().clone(), resources).await?;
    let server = match DemoServer::bind(config.port(), runtime.clone()).await {
        Ok(server) => server,
        Err(error) => {
            runtime.shutdown().await?;
            return Err(AppError::Server(error));
        }
    };

    println!("Safety Demo: {}", server.launch_url());
    let serve = server.serve();
    tokio::pin!(serve);
    let server_result = tokio::select! {
        result = &mut serve => result,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(AppError::ShutdownSignal)?;
            Ok(())
        }
    };
    let shutdown_result = runtime.shutdown().await;
    server_result.map_err(AppError::Server)?;
    shutdown_result.map_err(AppError::Workspace)
}

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    Config(#[from] cli::ConfigError),
    #[error(transparent)]
    Resources(#[from] ResourceError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("server failed: {0}")]
    Server(std::io::Error),
    #[error("shutdown signal failed: {0}")]
    ShutdownSignal(std::io::Error),
}
