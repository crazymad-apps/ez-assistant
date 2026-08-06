//! Safety Demo 的 Provider、真实工具与 ExecutionSpec 装配。

use std::{
    ffi::OsString,
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
    time::Duration,
};

use agent_context::ContextWindowEvaluator;
use agent_core::{
    ActiveGuardrailMode, ExecutionBudget, ExecutionSpec, GuardrailCheckConfig, GuardrailConfig,
    ModelRequestConfig,
};
use agent_model::{
    GenerationConfig, ModelService, ProviderOptions, ReasoningConfig, SystemPromptSnapshot,
};
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use agent_tools::{
    AbsolutePath, FsDeleteTool, FsEditTool, FsFindTool, FsListTool, FsReadTool, FsSearchTool,
    FsWriteTool, ReadFileToolConfig, SearchFilesToolConfig, SessionPathResolver, ShellExecTool,
    ShellExecToolConfig, ToolRegistry, ToolSetSnapshot,
};
use agent_tools_local::{
    EnvironmentPolicy, LocalFileSystem, LocalFileSystemConfig, LocalShell, LocalShellConfig,
};
use agent_types::ToolChoice;
use thiserror::Error;

const CONTEXT_THRESHOLD: f64 = 0.8;
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const MAX_TEXT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SHELL_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_RECORD_BYTES: u64 = 64 * 1024;
const MAX_SEARCH_STDERR_BYTES: u64 = 64 * 1024;

/// 每次 Run 复用的不可变模型、窗口判断和工具快照。
pub(crate) struct DemoResources {
    model: Arc<dyn ModelService>,
    context_window: Arc<ContextWindowEvaluator>,
    tools: ToolSetSnapshot,
    model_request: ModelRequestConfig,
}

/// 为真实 DeepSeek 会话冻结每个 Model Step 复用的显式 thinking 配置。
fn deepseek_model_request_config() -> ModelRequestConfig {
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert(
            "deepseek",
            serde_json::json!({"thinking": {"type": "enabled"}}),
        )
        .expect("static DeepSeek provider options are valid");
    ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: Some(ReasoningConfig { effort: None }),
        provider_options,
    }
}

impl DemoResources {
    pub(crate) fn from_environment(
        session_workdir: &AbsolutePath,
    ) -> Result<Arc<Self>, ResourceError> {
        let api_key = required_env("DEEPSEEK_API_KEY")?;
        let base_url = optional_env("DEEPSEEK_BASE_URL")
            .unwrap_or_else(|| "https://api.deepseek.com".to_owned());
        let model =
            optional_env("DEEPSEEK_MODEL").unwrap_or_else(|| "deepseek-v4-flash".to_owned());
        let context_window_tokens = optional_env("DEEPSEEK_CONTEXT_WINDOW_TOKENS").map_or(
            Ok(DEFAULT_CONTEXT_WINDOW_TOKENS),
            |value| {
                value.parse::<u64>().map_err(|_| {
                    ResourceError::Config(
                        "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be a positive integer".to_owned(),
                    )
                })
            },
        )?;
        if context_window_tokens == 0 {
            return Err(ResourceError::Config(
                "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be greater than zero".to_owned(),
            ));
        }
        let service = OpenAiCompatibleService::new(
            base_url,
            BearerCredential::new(api_key),
            model,
            context_window_tokens,
            Profile::deepseek(),
            TransportTimeouts::default(),
        )
        .map_err(|error| ResourceError::Provider(error.to_string()))?;
        Self::with_model_and_request(
            session_workdir,
            Arc::new(service),
            deepseek_model_request_config(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_model(
        session_workdir: &AbsolutePath,
        model: Arc<dyn ModelService>,
    ) -> Result<Arc<Self>, ResourceError> {
        Self::with_model_and_request(session_workdir, model, ModelRequestConfig::default())
    }

    fn with_model_and_request(
        session_workdir: &AbsolutePath,
        model: Arc<dyn ModelService>,
        model_request: ModelRequestConfig,
    ) -> Result<Arc<Self>, ResourceError> {
        let context_window = Arc::new(
            ContextWindowEvaluator::new(CONTEXT_THRESHOLD)
                .map_err(|error| ResourceError::Config(error.to_string()))?,
        );
        let tools = build_tools(session_workdir)?;
        Ok(Arc::new(Self {
            model,
            context_window,
            tools,
            model_request,
        }))
    }

    pub(crate) fn spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            system_prompt: SystemPromptSnapshot::new(vec![
                "You are running inside the ez-assistant Safety Demo.".to_owned(),
                "Use the dedicated file tools for file operations and shell only when a shell \
                 command is actually needed. Tool authorization is enforced by the host."
                    .to_owned(),
            ]),
            model: self.model.clone(),
            context_window: self.context_window.clone(),
            tools: self.tools.clone(),
            model_request: self.model_request.clone(),
            budget: ExecutionBudget::default(),
            guardrails: Some(GuardrailConfig {
                repeated_invocation: Some(GuardrailCheckConfig {
                    mode: ActiveGuardrailMode::Observe,
                    threshold: NonZeroU32::new(3).expect("three is non-zero"),
                }),
                consecutive_failures: Some(GuardrailCheckConfig {
                    mode: ActiveGuardrailMode::Enforce,
                    threshold: NonZeroU32::new(5).expect("five is non-zero"),
                }),
            }),
        }
    }
}

fn build_tools(session_workdir: &AbsolutePath) -> Result<ToolSetSnapshot, ResourceError> {
    let resolver = SessionPathResolver::new(session_workdir.clone());
    let filesystem = Arc::new(LocalFileSystem::new(LocalFileSystemConfig {
        max_text_file_bytes: NonZeroU64::new(MAX_TEXT_FILE_BYTES).expect("limit is non-zero"),
        ripgrep_program: OsString::from("rg"),
        max_search_stderr_bytes: NonZeroU64::new(MAX_SEARCH_STDERR_BYTES)
            .expect("limit is non-zero"),
    }));
    let read_config = ReadFileToolConfig::new(nonzero32(1), nonzero32(200), nonzero32(2_000))
        .map_err(|error| ResourceError::Tools(error.to_string()))?;
    let search_config = SearchFilesToolConfig::new(
        nonzero32(100),
        nonzero32(1_000),
        NonZeroU64::new(MAX_SEARCH_OUTPUT_BYTES).expect("limit is non-zero"),
        NonZeroU64::new(MAX_SEARCH_RECORD_BYTES).expect("limit is non-zero"),
    )
    .map_err(|error| ResourceError::Tools(error.to_string()))?;

    let shell = Arc::new(LocalShell::new(LocalShellConfig::new(
        EnvironmentPolicy::default(),
    )));
    let shell_config = ShellExecToolConfig::new(
        Duration::from_secs(120),
        Duration::from_secs(600),
        NonZeroU64::new(MAX_SHELL_OUTPUT_BYTES).expect("limit is non-zero"),
    )
    .map_err(|error| ResourceError::Tools(error.to_string()))?;

    let mut registry = ToolRegistry::new();
    registry
        .register(FsReadTool::new(
            filesystem.clone(),
            resolver.clone(),
            read_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsListTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsFindTool::new(
            filesystem.clone(),
            resolver.clone(),
            search_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsSearchTool::new(
            filesystem.clone(),
            resolver.clone(),
            search_config,
        ))
        .map_err(register_error)?;
    registry
        .register(FsWriteTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsEditTool::new(filesystem.clone(), resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(FsDeleteTool::new(filesystem, resolver.clone()))
        .map_err(register_error)?;
    registry
        .register(ShellExecTool::new(shell, resolver, shell_config))
        .map_err(register_error)?;
    Ok(registry.snapshot())
}

fn nonzero32(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("configured value is non-zero")
}

fn register_error(error: agent_tools::RegisterToolError) -> ResourceError {
    ResourceError::Tools(error.to_string())
}

fn required_env(name: &'static str) -> Result<String, ResourceError> {
    optional_env(name).ok_or_else(|| {
        ResourceError::Config(format!(
            "missing {name}; configure it in the repository .env"
        ))
    })
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Error)]
pub(crate) enum ResourceError {
    #[error("invalid demo configuration: {0}")]
    Config(String),
    #[error("provider setup failed: {0}")]
    Provider(String),
    #[error("tool setup failed: {0}")]
    Tools(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_demo_freezes_explicit_thinking_request_config() {
        let config = deepseek_model_request_config();
        assert_eq!(config.tool_choice, ToolChoice::Auto);
        assert_eq!(config.generation, GenerationConfig::default());
        assert_eq!(config.reasoning, Some(ReasoningConfig { effort: None }));
        assert_eq!(
            config.provider_options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
    }
}
