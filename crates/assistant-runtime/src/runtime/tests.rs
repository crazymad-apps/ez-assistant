use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelService,
    ModelStreamFuture, ModelTransportErrorKind, ReasoningConfig, SystemPromptSnapshot,
};
use agent_sdk::AllowAllAuthorizer;
use agent_testkit::{ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events};
use agent_tools::{
    ToolOutputChannel as AgentToolOutputChannel, ToolOutputChunk, ToolRegistry, ToolSetSnapshot,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason, MessageId, ModelIdentity,
    PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolChoice, ToolName, UserPart,
};
use assistant_protocol::{
    ConnectionValidationFailure, ConnectionValidationFailureKind, ConnectionValidationOutcome,
    RunId, ShutdownRuntimeRequest, SubmitInputRequest, ValidateModelConnectionRequest,
};
use serde_json::json;
use tokio::sync::{Barrier, Notify, broadcast::error::RecvError};

use super::connection_validation::{
    CONNECTION_VALIDATION_MAX_OUTPUT_TOKENS, CONNECTION_VALIDATION_PROMPT,
};
use super::*;
use crate::{
    ConfigSourceFailure, ConfigSourceFailureKind, ConfigSourceFuture, ConfigSourceLoad,
    ModelServiceFactoryError, ModelServiceFactoryRequest, RuntimeConfigSource,
    SystemPromptFactoryError,
};

const TEST_CONFIG: &str = r#"
schema_version = 1
default_model = "fixture"

[models.fixture]
protocol = "chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "fixture-model"
api_key = "unique-test-secret-9f1ca2"
context_window_tokens = 8192
max_output_tokens = 4096
"#;

struct MissingConfigSource;

impl RuntimeConfigSource for MissingConfigSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceLoad::Missing))
    }
}

struct UnavailableConfigSource;

impl RuntimeConfigSource for UnavailableConfigSource {
    fn display_path(&self) -> Option<String> {
        Some("/private/runtime/config.toml".to_owned())
    }

    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceLoad::Unavailable(
            ConfigSourceFailure::new(
                ConfigSourceFailureKind::Unsafe,
                "configuration source is unsafe",
            ),
        )))
    }
}

struct MutableConfigSource {
    document: Mutex<Option<String>>,
}

struct GatedConfigSource {
    document: String,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl RuntimeConfigSource for GatedConfigSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        let document = self.document.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            ConfigSourceLoad::Document(document)
        })
    }
}

impl MutableConfigSource {
    fn new(document: String) -> Self {
        Self {
            document: Mutex::new(Some(document)),
        }
    }

    fn replace(&self, document: Option<String>) {
        *self.document.lock().expect("source lock") = document;
    }
}

impl RuntimeConfigSource for MutableConfigSource {
    fn display_path(&self) -> Option<String> {
        Some("/private/runtime/config.toml".to_owned())
    }

    fn load(&self) -> ConfigSourceFuture<'_> {
        let document = self.document.lock().expect("source lock").clone();
        Box::pin(std::future::ready(match document {
            Some(document) => ConfigSourceLoad::Document(document),
            None => ConfigSourceLoad::Missing,
        }))
    }
}

struct CountingSystemPromptFactory {
    created: AtomicUsize,
}

impl CountingSystemPromptFactory {
    fn new() -> Self {
        Self {
            created: AtomicUsize::new(0),
        }
    }

    fn created(&self) -> usize {
        self.created.load(Ordering::Relaxed)
    }
}

impl SystemPromptFactory for CountingSystemPromptFactory {
    fn create_system_prompt(&self) -> Result<SystemPromptSnapshot, SystemPromptFactoryError> {
        let sequence = self.created.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(SystemPromptSnapshot::new(vec![format!(
            "Session prompt {sequence}"
        )]))
    }
}

struct StaticSystemPromptFactory;

impl SystemPromptFactory for StaticSystemPromptFactory {
    fn create_system_prompt(&self) -> Result<SystemPromptSnapshot, SystemPromptFactoryError> {
        Ok(SystemPromptSnapshot::new(vec![
            "Runtime test agent".to_owned(),
        ]))
    }
}

struct StaticModelFactory {
    model: Arc<dyn ModelService>,
}

impl StaticModelFactory {
    fn new(model: Arc<dyn ModelService>) -> Self {
        Self { model }
    }
}

impl ModelServiceFactory for StaticModelFactory {
    fn create_model(
        &self,
        _request: ModelServiceFactoryRequest<'_>,
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError> {
        Ok(self.model.clone())
    }
}

struct RecordingModelFactory {
    models: Mutex<VecDeque<Arc<dyn ModelService>>>,
    api_keys: Mutex<Vec<String>>,
}

impl RecordingModelFactory {
    fn new(models: impl IntoIterator<Item = Arc<dyn ModelService>>) -> Self {
        Self {
            models: Mutex::new(models.into_iter().collect()),
            api_keys: Mutex::new(Vec::new()),
        }
    }

    fn api_keys(&self) -> Vec<String> {
        self.api_keys.lock().expect("api key log").clone()
    }
}

impl ModelServiceFactory for RecordingModelFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError> {
        self.api_keys
            .lock()
            .expect("api key log")
            .push(request.api_key.to_owned());
        self.models
            .lock()
            .expect("model queue")
            .pop_front()
            .ok_or_else(|| ModelServiceFactoryError::new("fixture model queue is empty"))
    }
}

struct FailingModelFactory;

impl ModelServiceFactory for FailingModelFactory {
    fn create_model(
        &self,
        _request: ModelServiceFactoryRequest<'_>,
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError> {
        Err(ModelServiceFactoryError::new("fixture model build failed"))
    }
}

struct PanicModel {
    capabilities: ModelCapabilities,
}

impl ModelService for PanicModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        panic!("private model panic payload")
    }
}

/// 在建流阶段等待 Runtime 取消，用于验证受控关闭传播。
struct CancellationAwareModel {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
}

impl ModelService for CancellationAwareModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            context.cancellation.cancelled().await;
            Err(ModelError::Cancelled)
        })
    }
}

/// 故意忽略取消且永不完成，用于证明 Runtime 的整体验证超时不依赖 Adapter。
struct NeverModel {
    capabilities: ModelCapabilities,
}

/// 建流时发出信号后永不完成，故意违反取消契约以验证 Runtime 关闭兜底。
struct EnteredNeverModel {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
}

impl ModelService for EnteredNeverModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.entered.notify_one();
        Box::pin(std::future::pending())
    }
}

impl ModelService for NeverModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(std::future::pending())
    }
}

fn runtime(model: Arc<dyn ModelService>) -> AssistantRuntime {
    runtime_with_tools_and_capacity(model, ToolSetSnapshot::default(), 32)
}

fn runtime_with_tools(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> AssistantRuntime {
    runtime_with_tools_and_capacity(model, tools, 32)
}

fn runtime_with_tools_and_capacity(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    event_capacity: usize,
) -> AssistantRuntime {
    runtime_with_factories(
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        tools,
        event_capacity,
    )
}

fn runtime_with_factories(
    model_factory: Arc<dyn ModelServiceFactory>,
    system_prompt_factory: Arc<dyn SystemPromptFactory>,
    tools: ToolSetSnapshot,
    event_capacity: usize,
) -> AssistantRuntime {
    runtime_with_factories_and_config(
        model_factory,
        system_prompt_factory,
        tools,
        RuntimeConfig::new(NonZeroUsize::new(event_capacity).expect("non-zero")),
    )
}

fn runtime_with_factories_and_config(
    model_factory: Arc<dyn ModelServiceFactory>,
    system_prompt_factory: Arc<dyn SystemPromptFactory>,
    tools: ToolSetSnapshot,
    config: RuntimeConfig,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::new(
        config,
        Arc::new(MissingConfigSource),
        model_factory,
        system_prompt_factory,
        tools,
        Arc::new(AllowAllAuthorizer),
    );
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
}

async fn runtime_with_store(
    model: Arc<dyn ModelService>,
    store: Arc<dyn RuntimeStore>,
    config: RuntimeConfig,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::open(
        config,
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        Arc::new(AllowAllAuthorizer),
        store,
    )
    .await
    .expect("open test runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
}

fn empty_model() -> Arc<dyn ModelService> {
    Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [],
    ))
}

fn config_with_api_key(api_key: &str) -> String {
    TEST_CONFIG.replace("unique-test-secret-9f1ca2", api_key)
}

fn model_capabilities(has_tools: bool) -> ModelCapabilities {
    ModelCapabilities {
        reasoning: false,
        tool_calls: has_tools,
        streaming: true,
    }
}

fn assistant_text(message_id: &str, text: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new(format!("{message_id}-text")).expect("part id"),
            text: text.to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

fn assistant_tool_call(message_id: &str, tool_name: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("call-1").expect("tool call id"),
            name: ToolName::new(tool_name).expect("tool name"),
            arguments: json!({"value": "hello"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn hanging_runtime(
    hanging_count: usize,
    final_text: Option<&str>,
    entered: Arc<Notify>,
    cleanup: Arc<Notify>,
) -> AssistantRuntime {
    let mut registry = ToolRegistry::new();
    registry
        .register(
            ScriptedTool::hanging("slow_tool", OrderLog::new())
                .with_entered_signal(entered)
                .with_cleanup_signal(cleanup),
        )
        .expect("register hanging tool");
    let tool_message = assistant_tool_call("assistant-tools", "slow_tool");
    let mut scripts = (0..hanging_count)
        .map(|_| ModelScript::Events(message_events(&tool_message)))
        .collect::<Vec<_>>();
    if let Some(text) = final_text {
        scripts.push(ModelScript::Events(message_events(&assistant_text(
            "assistant-final",
            text,
        ))));
    }
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        scripts,
    ));
    runtime_with_tools(model, registry.snapshot())
}

async fn wait_for_terminal(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
    run_id: &RunId,
) -> assistant_protocol::RunSnapshot {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })
                .await
                .expect("run query")
                .run;
            if snapshot.status.is_terminal() {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run reaches terminal state")
}

mod concurrency;
mod config;
mod connection_validation;
mod failures;
mod input;
mod runs;
mod session_management;
mod sessions;
mod store;
