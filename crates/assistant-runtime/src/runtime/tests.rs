use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelService,
    ModelServiceBundle, ModelStreamFuture, ModelTransportErrorKind, ReasoningConfig,
    SystemPromptSnapshot,
};
use agent_testkit::{ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events};
use agent_tools::{
    ToolOutputChannel as AgentToolOutputChannel, ToolOutputChunk, ToolRegistry, ToolSetSnapshot,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason, MessageId, ModelIdentity,
    PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolChoice, ToolName, UserPart,
};
use assistant_protocol::{
    ApprovalSnapshot, ConnectionValidationFailure, ConnectionValidationFailureKind,
    ConnectionValidationOutcome, ListPendingApprovalsRequest, ModelConnectionTarget, RunId,
    SetSessionApprovalModeRequest, ShutdownRuntimeRequest, SubmitInputRequest,
    ValidateModelConnectionRequest,
};
use serde_json::json;
use tokio::sync::{Barrier, Notify, broadcast::error::RecvError};

use super::connection_validation::{
    CONNECTION_VALIDATION_MAX_OUTPUT_TOKENS, CONNECTION_VALIDATION_PROMPT,
};
use super::*;
use crate::{
    ChildTaskWorkspaceError, ChildTaskWorkspaceFactory, ChildTaskWorkspaceFuture,
    ChildTaskWorkspaceLease, ConfigDocument, ConfigSourceFailure, ConfigSourceFailureKind,
    ConfigSourceFuture, ConfigSourceLoad, ConfigSourceReplace, ConfigSourceReplaceFuture,
    ForkSessionEnvironmentFactoryRequest, ModelServiceFactoryError, ModelServiceFactoryRequest,
    PreparedSessionEnvironment, RunToolBundle, RunToolFactory, RunToolFactoryError,
    RunToolFactoryErrorKind, RuntimeConfigSource, SessionEnvironmentFactoryError,
    SessionExecutionEnvironment,
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

struct StaticRunToolFactory {
    tools: ToolSetSnapshot,
}

#[derive(Default)]
struct TestChildWorkspaceFactory {
    released_paths: Arc<Mutex<Vec<String>>>,
}

struct TestChildWorkspaceLease {
    path: String,
    directory: Option<tempfile::TempDir>,
    released_paths: Arc<Mutex<Vec<String>>>,
}

impl ChildTaskWorkspaceLease for TestChildWorkspaceLease {
    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for TestChildWorkspaceLease {
    fn drop(&mut self) {
        // 先释放 TempDir，再记录已清理路径，使测试观察到的状态与产品 lease 一致。
        drop(self.directory.take());
        self.released_paths
            .lock()
            .expect("released path log")
            .push(self.path.clone());
    }
}

impl ChildTaskWorkspaceFactory for TestChildWorkspaceFactory {
    fn create<'a>(
        &'a self,
        _child_task_id: &'a assistant_protocol::ChildTaskId,
    ) -> ChildTaskWorkspaceFuture<'a> {
        let released_paths = self.released_paths.clone();
        Box::pin(async move {
            let directory = tempfile::tempdir().map_err(ChildTaskWorkspaceError::with_source)?;
            let path = directory.path().to_string_lossy().into_owned();
            Ok(Box::new(TestChildWorkspaceLease {
                path,
                directory: Some(directory),
                released_paths,
            }) as Box<dyn ChildTaskWorkspaceLease>)
        })
    }
}

impl RunToolFactory for StaticRunToolFactory {
    fn compile(
        &self,
        _request: crate::RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        Ok(RunToolBundle::new(self.tools.clone(), Vec::new()))
    }
}

fn static_run_tool_factory(tools: ToolSetSnapshot) -> Arc<dyn RunToolFactory> {
    Arc::new(StaticRunToolFactory { tools })
}

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
            ConfigSourceLoad::Document(test_config_document(document))
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
            Some(document) => ConfigSourceLoad::Document(test_config_document(document)),
            None => ConfigSourceLoad::Missing,
        }))
    }

    fn replace(
        &self,
        expected_revision: Option<String>,
        document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        let mut current = self.document.lock().expect("source lock");
        let observed = current.as_ref().map(|value| test_config_revision(value));
        if observed != expected_revision {
            let load = current
                .clone()
                .map(test_config_document)
                .map(ConfigSourceLoad::Document)
                .unwrap_or(ConfigSourceLoad::Missing);
            return Box::pin(std::future::ready(ConfigSourceReplace::Conflict(load)));
        }
        *current = Some(document.clone());
        Box::pin(std::future::ready(ConfigSourceReplace::Applied(
            test_config_document(document),
        )))
    }
}

fn test_config_document(document: String) -> ConfigDocument {
    let revision = test_config_revision(&document);
    ConfigDocument::new(document, revision)
}

fn test_config_revision(document: &str) -> String {
    let mut hasher = DefaultHasher::new();
    document.hash(&mut hasher);
    format!("test-{:x}", hasher.finish())
}

fn configured_validation_request() -> ValidateModelConnectionRequest {
    ValidateModelConnectionRequest {
        target: ModelConnectionTarget::Configured {
            model_key: assistant_protocol::ModelKey::new("fixture").expect("model key"),
        },
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

impl SessionEnvironmentFactory for CountingSystemPromptFactory {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        let sequence = self.created.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(test_environment(
            request,
            SystemPromptSnapshot::new(vec![format!("Session prompt {sequence}")]),
        ))
    }

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        self.created.fetch_add(1, Ordering::Relaxed);
        Ok(test_fork_environment(request))
    }
}

struct StaticSystemPromptFactory;

impl SessionEnvironmentFactory for StaticSystemPromptFactory {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        Ok(test_environment(
            request,
            SystemPromptSnapshot::new(vec!["Runtime test agent".to_owned()]),
        ))
    }

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        Ok(test_fork_environment(request))
    }
}

fn test_fork_environment(
    request: ForkSessionEnvironmentFactoryRequest<'_>,
) -> PreparedSessionEnvironment {
    let private = format!("/runtime/sessions/{}/private", request.session_id);
    let attachment = format!("/runtime/sessions/{}/attachments", request.session_id);
    let tool_images = format!("/runtime/sessions/{}/tool-images", request.session_id);
    let mut parts = request.source_system_prompt.parts().to_vec();
    if let Some(directory_prompt) = parts.last_mut() {
        *directory_prompt = format!("Session directories for {}", request.session_id);
    }
    PreparedSessionEnvironment {
        system_prompt: SystemPromptSnapshot::new(parts),
        environment: SessionExecutionEnvironment {
            workspace_id: request.source_environment.workspace_id.clone(),
            working_directory: request.source_environment.working_directory.clone(),
            workspace_private_directory: request
                .source_environment
                .workspace_private_directory
                .clone(),
            session_attachment_directory: attachment,
            session_tool_image_directory: tool_images,
            session_private_directory: private,
        },
    }
}

fn test_environment(
    request: SessionEnvironmentFactoryRequest<'_>,
    system_prompt: SystemPromptSnapshot,
) -> PreparedSessionEnvironment {
    let private = format!("/runtime/sessions/{}/private", request.session_id);
    let attachment = format!("/runtime/sessions/{}/attachments", request.session_id);
    let tool_images = format!("/runtime/sessions/{}/tool-images", request.session_id);
    let (workspace_id, working_directory, workspace_private_directory) = match request.workspace {
        Some(workspace) => (
            Some(workspace.workspace_id.clone()),
            workspace.user_directory.to_owned(),
            Some(workspace.agent_directory.to_owned()),
        ),
        None => (None, private.clone(), None),
    };
    PreparedSessionEnvironment {
        system_prompt,
        environment: SessionExecutionEnvironment {
            workspace_id,
            working_directory,
            workspace_private_directory,
            session_attachment_directory: attachment,
            session_tool_image_directory: tool_images,
            session_private_directory: private,
        },
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
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        Ok(ModelServiceBundle::text_only(self.model.clone()))
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
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        self.api_keys
            .lock()
            .expect("api key log")
            .push(request.api_key.to_owned());
        self.models
            .lock()
            .expect("model queue")
            .pop_front()
            .map(ModelServiceBundle::text_only)
            .ok_or_else(|| ModelServiceFactoryError::new("fixture model queue is empty"))
    }
}

struct FailingModelFactory;

impl ModelServiceFactory for FailingModelFactory {
    fn create_model(
        &self,
        _request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
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

fn runtime_with_run_tool_factory(
    model: Arc<dyn ModelService>,
    run_tool_factory: Arc<dyn RunToolFactory>,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        run_tool_factory,
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
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
    system_prompt_factory: Arc<dyn SessionEnvironmentFactory>,
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
    system_prompt_factory: Arc<dyn SessionEnvironmentFactory>,
    tools: ToolSetSnapshot,
    config: RuntimeConfig,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::new(
        config,
        Arc::new(MissingConfigSource),
        model_factory,
        system_prompt_factory,
        static_run_tool_factory(tools),
        Arc::new(TestChildWorkspaceFactory::default()),
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
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store,
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .expect("open test runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    runtime
}

async fn runtime_with_store_and_child_workspaces(
    model: Arc<dyn ModelService>,
    store: Arc<dyn RuntimeStore>,
    workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
) -> AssistantRuntime {
    let runtime = AssistantRuntime::open(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        workspace_factory,
        store,
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .expect("open child-capable test runtime");
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

fn model_input(
    key: &str,
    endpoint: &str,
    credential: assistant_protocol::ModelCredentialChange,
) -> assistant_protocol::ModelConfigurationInput {
    assistant_protocol::ModelConfigurationInput {
        model_key: assistant_protocol::ModelKey::new(key).expect("model key"),
        display_name: key.to_owned(),
        protocol: "chat_completions".to_owned(),
        provider: "fixture".to_owned(),
        endpoint: endpoint.to_owned(),
        model: format!("{key}-model"),
        context_window_tokens: 8_192,
        max_output_tokens: 4_096,
        credential,
    }
}

fn model_capabilities(has_tools: bool) -> ModelCapabilities {
    ModelCapabilities {
        reasoning: false,
        image_input: false,
        tool_calls: has_tools,
        multimodal_tool_result: false,
        tool_choice: if has_tools {
            agent_model::ToolChoiceCapabilities::all()
        } else {
            agent_model::ToolChoiceCapabilities::default()
        },
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

async fn set_auto_approval(runtime: &AssistantRuntime, session_id: &SessionId) {
    runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session_id.clone(),
            approval_mode: assistant_protocol::ApprovalMode::Auto,
        })
        .await
        .expect("set automatic approval mode");
}

async fn wait_for_pending_approval(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
) -> ApprovalSnapshot {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let mut approvals = runtime
                .list_pending_approvals(ListPendingApprovalsRequest {
                    session_id: session_id.clone(),
                })
                .expect("list pending approvals")
                .approvals;
            if let Some(approval) = approvals.pop() {
                return approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("approval becomes pending")
}

mod approval;
mod attachment;
mod concurrency;
mod config;
mod connection_validation;
mod delegation;
mod failures;
mod input;
mod memory;
mod permission;
mod product;
mod runs;
mod session_management;
mod sessions;
mod store;
mod workspace;
