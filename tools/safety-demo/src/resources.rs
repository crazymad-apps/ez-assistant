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
};
use agent_model::{
    ModelCallContext, ModelError, ModelRequest, ModelService, ModelStreamFuture, ReasoningConfig,
    SystemPromptSnapshot,
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
}

/// 为 Safety Demo 的 DeepSeek 服务冻结 thinking 模式请求配置。
///
/// Core 保持 Provider-neutral，不按 Provider 名称注入私有参数；Demo 在装配真实服务时
/// 用该薄包装器保证每个模型 Step 都显式携带 `thinking.type = enabled`。
struct DeepSeekThinkingModel {
    inner: Arc<dyn ModelService>,
}

impl DeepSeekThinkingModel {
    fn new(inner: Arc<dyn ModelService>) -> Self {
        Self { inner }
    }
}

impl ModelService for DeepSeekThinkingModel {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(
        &self,
        mut request: ModelRequest,
        context: ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        if let Err(error) = configure_deepseek_thinking(&mut request) {
            return Box::pin(async move { Err(error) });
        }
        self.inner.stream(request, context)
    }
}

fn configure_deepseek_thinking(request: &mut ModelRequest) -> Result<(), ModelError> {
    let mut options = request
        .provider_options
        .get("deepseek")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let enabled = serde_json::json!({"type": "enabled"});
    if let Some(existing) = options.get("thinking")
        && existing != &enabled
    {
        return Err(ModelError::Config(
            "Safety Demo requires DeepSeek thinking mode to remain enabled".to_owned(),
        ));
    }
    options.insert("thinking".to_owned(), enabled);
    request
        .reasoning
        .get_or_insert(ReasoningConfig { effort: None });
    request
        .provider_options
        .insert("deepseek", serde_json::Value::Object(options))
        .map_err(|error| ModelError::Config(error.to_string()))
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
        let service = Arc::new(service) as Arc<dyn ModelService>;
        let model = Arc::new(DeepSeekThinkingModel::new(service));
        Self::with_model(session_workdir, model)
    }

    pub(crate) fn with_model(
        session_workdir: &AbsolutePath,
        model: Arc<dyn ModelService>,
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
    use agent_model::{
        GenerationConfig, ModelCapabilities, ModelTransportErrorKind, ProviderOptions,
    };
    use agent_testkit::{ModelScript, ScriptedModelService};
    use agent_types::{ConversationSnapshot, ToolChoice};

    use super::*;

    fn request() -> ModelRequest {
        ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }

    #[test]
    fn deepseek_demo_explicitly_enables_thinking_and_preserves_other_options() {
        let mut request = request();
        request
            .provider_options
            .insert("deepseek", serde_json::json!({"demo_marker": true}))
            .expect("valid provider options");

        configure_deepseek_thinking(&mut request).expect("configure thinking");

        assert_eq!(request.reasoning, Some(ReasoningConfig { effort: None }));
        assert_eq!(
            request.provider_options.get("deepseek"),
            Some(&serde_json::json!({
                "demo_marker": true,
                "thinking": {"type": "enabled"}
            }))
        );
    }

    #[test]
    fn deepseek_demo_rejects_a_conflicting_thinking_mode() {
        let mut request = request();
        request
            .provider_options
            .insert(
                "deepseek",
                serde_json::json!({"thinking": {"type": "disabled"}}),
            )
            .expect("valid provider options");

        assert!(matches!(
            configure_deepseek_thinking(&mut request),
            Err(ModelError::Config(message)) if message.contains("remain enabled")
        ));
    }

    #[tokio::test]
    async fn deepseek_wrapper_configures_every_delegated_request() {
        let inner = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: true,
                tool_calls: true,
                streaming: true,
            },
            128_000,
            [ModelScript::FailEstablishment(ModelError::Transport {
                kind: ModelTransportErrorKind::Connection,
                message: "offline fixture".to_owned(),
            })],
        ));
        let model = DeepSeekThinkingModel::new(inner.clone());

        assert!(matches!(
            model.stream(request(), ModelCallContext::default()).await,
            Err(ModelError::Transport { message, .. }) if message == "offline fixture"
        ));
        let requests = inner.take_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].provider_options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
        assert_eq!(
            requests[0].reasoning,
            Some(ReasoningConfig { effort: None })
        );
    }
}
