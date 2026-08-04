//! 记忆能力的独立开发验证宿主。
//!
//! 该 binary 由开发者显式启动，不属于正式 Assistant Runtime 或桌面应用。

mod atomic_json;
mod chat;
mod cli;
mod config;
mod input;
mod journal;
mod pinned_store;
mod recall_source;
mod resources;
mod session;

use thiserror::Error;

#[derive(Debug, Error)]
enum DemoError {
    #[error("CLI error: {0}")]
    Cli(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("memory error: {0}")]
    Memory(String),
    #[error("session error: {0}")]
    Session(String),
    #[error("tool registration error: {0}")]
    ToolRegistration(String),
    #[error("execution error: {0}")]
    Execution(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl DemoError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Cli(_) | Self::Config(_) => 2,
            _ => 1,
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}

async fn run() -> Result<(), DemoError> {
    match cli::parse_env()? {
        cli::Command::Help => println!("{}", cli::HELP),
        cli::Command::Chat { data_dir, session } => {
            // 只有 chat 入口读取 .env；--help 和全部单测不触碰 credential。
            dotenvy::dotenv().ok();
            let config = config::load_chat_config()?;
            let resources = resources::build_chat_resources(&data_dir, config).await?;
            chat::run_chat(data_dir, session, resources).await?;
        }
    }
    Ok(())
}
