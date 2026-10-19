//! Run 与连接验证共用的模型服务编译。

use std::{sync::Arc, time::Duration};

use agent_core::{ExecutionBudget, ToolAuthorizer};
use agent_model::{
    ModelAttemptEvent, ModelAttemptObserver, ModelError, ModelImagePreparation,
    ModelImagePreprocessor, ModelImageResource, ModelService, ModelStreamFuture, ProviderOptions,
    ReasoningConfig, RetryingModelService, SystemPromptSnapshot,
};
use agent_sdk::{Agent, AgentBuilder};
use agent_tools::{
    ImageInspection, ImageInspectionFuture, ImageInspector, ImageInspectorError,
    InspectImagesRequest,
};
use agent_types::ToolChoice;
use assistant_protocol::AgentVariant;

use super::AssistantRuntime;
use super::channel::SpeakTool;
use super::tool_assembly::{RunToolAssembly, RunToolContribution};
use crate::{
    ChildTaskWorkspaceFactory, ModelProtocol, ModelServiceFactoryRequest,
    ResolvedModelCapabilities, RunToolFactory, RunToolFactoryErrorKind, RuntimeError,
    RuntimeResult, RuntimeStore,
    config::{ConfigSnapshot, ResolvedModelConfig},
    context_compaction::RuntimeContextCompactor,
    delegation::{
        ChildTaskRegistry, DelegateTaskTool, ParentDelegationController, ParentDelegationResources,
    },
    goal::{GoalRunBinding, GoalRunSignalLatch, GoalState, UpdateGoalTool},
    mcp::{CallMcpTool, DiscoverMcpTools, McpImageMaterializer, McpRegistry, McpRunDisclosure},
    observation::ObservationCoordinator,
    permission::{
        ApprovalRegistry, PermissionCoordinator, RunAuthorizationScope, RuntimeApprovalResolver,
        RuntimeToolAuthorizer,
    },
    session::SessionController,
    skill::{LoadSkillTool, SkillActivationLatch, SkillActivationOwner},
    work_plan::UpdatePlanTool,
};

/// 现有稳定 owner 直接派生路由；不持久化第二份映射，不依赖配置或正文周期。
pub(super) fn conversation_routing_id(owner: &assistant_protocol::ConversationOwner) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"ez-assistant/conversation-routing/v1\0");
    let (session, child) = match owner {
        assistant_protocol::ConversationOwner::MainSession { session_id } => (session_id, None),
        assistant_protocol::ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        } => (session_id, Some(child_task_id)),
    };
    hash.update((session.as_str().len() as u64).to_be_bytes());
    hash.update(session.as_str().as_bytes());
    if let Some(child) = child {
        hash.update(b"child\0");
        hash.update(child.as_str().as_bytes());
    } else {
        hash.update(b"main\0");
    }
    format!("{:x}", hash.finalize())
}

/// 一次配置快照编译出的模型调用边界。
///
/// Run 和连接验证共用这条构造链，避免两者对 endpoint、credential、协议 Adapter、
/// timeout 和 retry 产生不同解释；两者的请求内容仍分别构造。
pub(super) struct CompiledModelService {
    input_token_count_supported: bool,
    pub(super) model: Arc<dyn ModelService>,
    /// 压缩与子任务模板使用无父观察器服务；实际 child 始终派生独立 owner 服务。
    background_model: Arc<dyn ModelService>,
    pub(super) provider: agent_types::ProviderId,
    pub(super) protocol: ModelProtocol,
    pub(super) capabilities: ResolvedModelCapabilities,
    pub(super) max_output_tokens: u32,
    pub(super) request_timeout: Duration,
    pub(super) image_preprocessor: Option<Arc<dyn ModelImagePreprocessor>>,
}

/// 父 Run 冻结的模型装配输入；每个 child owner/正文 generation 派生服务，
/// 不重新选择配置，不复制 Journal，压缩服务仍保持无状态。
pub(crate) struct OwnerModelBinding {
    snapshot: Arc<ConfigSnapshot>,
    model: ResolvedModelConfig,
    factory: Arc<dyn crate::ModelServiceFactory>,
    environment: crate::SessionExecutionEnvironment,
    owner: String,
    store: Arc<dyn RuntimeStore>,
    session: Arc<SessionController>,
    observer: Option<Arc<dyn ModelAttemptObserver>>,
    auxiliary: Option<Arc<AuxiliaryVisionBinding>>,
}
impl OwnerModelBinding {
    pub(crate) fn bind_child_agent(
        &self,
        agent: &Agent,
        child: &assistant_protocol::ChildTaskId,
        model: Arc<dyn ModelService>,
    ) -> RuntimeResult<Agent> {
        let mut agent = agent
            .try_with_model(model)
            .map_err(|source| RuntimeError::AgentBuildFailed { source })?;
        if let Some(auxiliary) = &self.auxiliary {
            let inspector =
                auxiliary.compile(&assistant_protocol::ConversationOwner::ChildTask {
                    session_id: self.session.id().clone(),
                    child_task_id: child.clone(),
                })?;
            agent = agent
                .try_with_tool(inspection_tool(inspector, &self.environment)?)
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "child vision tool",
                })?;
        }
        Ok(agent)
    }

    pub(crate) fn compile_child(
        &self,
        child: &assistant_protocol::ChildTaskId,
        generation: u64,
    ) -> RuntimeResult<Arc<dyn ModelService>> {
        self.compile(
            &format!("{}:child:{}:{generation}", self.owner, child.as_str()),
            None,
            assistant_protocol::ConversationOwner::ChildTask {
                session_id: self.session.id().clone(),
                child_task_id: child.clone(),
            },
        )
    }
    pub(super) fn compile_main(&self, generation: u64) -> RuntimeResult<Arc<dyn ModelService>> {
        self.compile(
            &format!("{}:{generation}", self.owner),
            self.observer.clone(),
            assistant_protocol::ConversationOwner::MainSession {
                session_id: self.session.id().clone(),
            },
        )
    }
    fn compile(
        &self,
        owner: &str,
        observer: Option<Arc<dyn ModelAttemptObserver>>,
        conversation_owner: assistant_protocol::ConversationOwner,
    ) -> RuntimeResult<Arc<dyn ModelService>> {
        let mut compiled = compile_resolved_model_service_with_history(
            &self.snapshot,
            &self.model,
            self.factory.as_ref(),
            observer,
            Some(owner),
            Some(owner.rsplit_once(':').map_or(owner, |(base, _)| base)),
            &conversation_routing_id(&conversation_owner),
        )?;
        bind_image_preparation(
            &mut compiled,
            &self.environment,
            Some((self.store.clone(), conversation_owner)),
            Some(self.session.clone()),
        );
        Ok(compiled.model)
    }
}

mod history_recovery;
use history_recovery::CallScopedHistoryModelService;
#[cfg(test)]
use history_recovery::RecoveringHistoryModelService;

/// 未启用重试时只补充 attempt 观察，不改变下层取消、超时或建流语义。
struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    observer: Arc<dyn ModelAttemptObserver>,
}

/// 在协议服务与有限重试之外一次性准备本次调用需要的全部图片。
struct RequestPreparingModelService {
    inner: Arc<dyn ModelService>,
    preprocessor: Option<Arc<dyn ModelImagePreprocessor>>,
    tool_image_directory: String,
    native_history: Option<(Arc<dyn RuntimeStore>, assistant_protocol::ConversationOwner)>,
    measurement_session: Option<Arc<SessionController>>,
    session_binding: Option<String>,
    input_token_count_supported: bool,
}

struct AuxiliaryVisionBinding {
    snapshot: Arc<ConfigSnapshot>,
    model: ResolvedModelConfig,
    factory: Arc<dyn crate::ModelServiceFactory>,
    environment: crate::SessionExecutionEnvironment,
    selection: assistant_protocol::ModelSelection,
    timeout: Duration,
    output_budget: u32,
}

impl AuxiliaryVisionBinding {
    fn compile(
        &self,
        owner: &assistant_protocol::ConversationOwner,
    ) -> RuntimeResult<agent_tools::SharedImageInspector> {
        let mut auxiliary = compile_resolved_model_service(
            &self.snapshot,
            &self.model,
            self.factory.as_ref(),
            None,
            &conversation_routing_id(owner),
        )?;
        if !auxiliary.capabilities.image_input {
            return Err(RuntimeError::InvalidRequest {
                reason: "辅助模型不支持图片输入，请重新选择。",
            });
        }
        let image_preprocessor =
            auxiliary
                .image_preprocessor
                .clone()
                .ok_or(RuntimeError::InvalidRequest {
                    reason: "辅助模型缺少图片预处理能力。",
                })?;
        bind_image_preparation(&mut auxiliary, &self.environment, None, None);
        let (reasoning, provider_options) = protocol_request_options(
            &auxiliary.provider,
            auxiliary.protocol,
            &auxiliary.capabilities,
            None,
        )?;
        Ok(Arc::new(AuxiliaryVisionInspector {
            selection: self.selection.clone(),
            model: auxiliary.model,
            image_preprocessor,
            reasoning,
            provider_options,
            timeout: self.timeout,
            max_output_tokens: self.output_budget.min(auxiliary.max_output_tokens),
        }))
    }
}

fn inspection_tool(
    inspector: agent_tools::SharedImageInspector,
    environment: &crate::SessionExecutionEnvironment,
) -> RuntimeResult<agent_tools::InspectImagesTool> {
    let workdir = agent_tools::AbsolutePath::new(&environment.working_directory).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "vision tool working directory",
        }
    })?;
    Ok(agent_tools::InspectImagesTool::new(
        inspector,
        agent_tools::SessionPathResolver::new(workdir),
    ))
}

struct AuxiliaryVisionInspector {
    selection: assistant_protocol::ModelSelection,
    model: Arc<dyn ModelService>,
    image_preprocessor: Arc<dyn ModelImagePreprocessor>,
    reasoning: Option<ReasoningConfig>,
    provider_options: ProviderOptions,
    timeout: Duration,
    max_output_tokens: u32,
}

impl ImageInspector for AuxiliaryVisionInspector {
    fn inspect<'a>(
        &'a self,
        input: InspectImagesRequest,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> ImageInspectionFuture<'a> {
        Box::pin(async move {
            let started_at = std::time::Instant::now();
            if cancellation.is_cancelled() {
                return Err(ImageInspectorError::Cancelled);
            }
            let child = cancellation.child_token();
            let mut prepared_images = agent_model::PreparedModelImages::default();
            for path in &input.image_paths {
                let resource = ModelImageResource::LocalFile { path: path.clone() };
                match self
                    .image_preprocessor
                    .prepare(&resource, &child)
                    .await
                    .map_err(map_inspector_error)?
                {
                    ModelImagePreparation::Image(image) => {
                        prepared_images.insert_file_reference(path.clone(), image);
                    }
                    ModelImagePreparation::NotImage => return Err(ImageInspectorError::Failed),
                }
            }
            let mut prompt = format!("Inspection goal: {}", input.goal);
            if let Some(background) = input.background {
                prompt.push_str("\nOptional background: ");
                prompt.push_str(&background);
            }
            prompt.push_str(
                "\nReturn direct findings. Include relevant OCR, key observations, and uncertainties when applicable.",
            );
            let files = input
                .image_paths
                .into_iter()
                .map(|readable_path| agent_types::FileReference {
                    original_name: std::path::Path::new(&readable_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("image")
                        .to_owned(),
                    readable_path,
                })
                .collect();
            let request = agent_model::ModelRequest {
                system: SystemPromptSnapshot::new(vec![
                    "You are an image inspection model. Analyze only the supplied images and answer the stated goal without inventing missing context.".to_owned(),
                ]),
                conversation: agent_types::ConversationSnapshot::new(vec![
                    agent_types::ConversationMessage::User(agent_types::UserMessage {
                        origin: Default::default(),
                        transcript_visibility: Default::default(),
                        id: agent_types::MessageId::new("auxiliary-vision-user")
                            .expect("static message id"),
                        parts: vec![
                            agent_types::UserPart::Text(agent_types::TextPart {
                                id: agent_types::PartId::new("auxiliary-vision-goal")
                                    .expect("static part id"),
                                text: prompt,
                            }),
                            agent_types::UserPart::FileReferences(
                                agent_types::FileReferencesPart {
                                    id: agent_types::PartId::new("auxiliary-vision-images")
                                        .expect("static part id"),
                                    files,
                                },
                            ),
                        ],
                    }),
                ]),
                tools: Vec::new(),
                tool_choice: ToolChoice::None,
                generation: agent_model::GenerationConfig {
                    temperature: None,
                    top_p: None,
                    max_output_tokens: Some(self.max_output_tokens),
                    stop: Vec::new(),
                },
                reasoning: self.reasoning.clone(),
                provider_options: self.provider_options.clone(),
            };
            let consume = async {
                let mut context = agent_model::ModelCallContext::new(child.clone());
                context.prepared_images = prepared_images;
                let mut stream = self
                    .model
                    .stream(request, context)
                    .await
                    .map_err(map_inspector_error)?;
                use futures_util::StreamExt as _;
                while let Some(event) = stream.next().await {
                    match event {
                        agent_model::ModelEvent::TurnFinished { message } => {
                            let usage = message.usage.clone();
                            let text = message
                                .parts
                                .into_iter()
                                .filter_map(|part| match part {
                                    agent_types::AssistantPart::Text(part) => Some(part.text),
                                    _ => None,
                                })
                                .collect::<String>();
                            if text.trim().is_empty() {
                                return Err(ImageInspectorError::Failed);
                            }
                            return Ok(ImageInspection {
                                text,
                                model_provider: self
                                    .selection
                                    .provider_instance_id
                                    .as_str()
                                    .to_owned(),
                                model_id: self.selection.model_id.clone(),
                                elapsed_ms: u64::try_from(started_at.elapsed().as_millis())
                                    .unwrap_or(u64::MAX),
                                usage,
                            });
                        }
                        agent_model::ModelEvent::TurnFailed { error } => {
                            return Err(map_inspector_error(error));
                        }
                        _ => {}
                    }
                }
                Err(ImageInspectorError::Failed)
            };
            tokio::select! {
                () = cancellation.cancelled() => {
                    child.cancel();
                    Err(ImageInspectorError::Cancelled)
                }
                result = tokio::time::timeout(self.timeout, consume) => match result {
                    Ok(result) => result,
                    Err(_) => {
                        child.cancel();
                        Err(ImageInspectorError::Timeout)
                    }
                }
            }
        })
    }
}

fn map_inspector_error(error: ModelError) -> ImageInspectorError {
    if matches!(error, ModelError::Cancelled) {
        ImageInspectorError::Cancelled
    } else {
        ImageInspectorError::Failed
    }
}

impl RequestPreparingModelService {
    async fn measurement_history(&self) -> Option<[u8; 32]> {
        let (store, owner) = self.native_history.as_ref()?;
        let history = match owner {
            assistant_protocol::ConversationOwner::MainSession { session_id } => {
                store.load_conversation(session_id).await
            }
            assistant_protocol::ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                store
                    .load_child_conversation(session_id, child_task_id)
                    .await
            }
        }
        .ok()?;
        crate::session::context_history_hash(&history)
    }

    async fn prepare_request(
        &self,
        request: &agent_model::ModelRequest,
    ) -> Result<agent_model::ModelRequest, ModelError> {
        if self.inner.context_policy().mode()
            != agent_model::ContextManagementMode::ServerCompaction
        {
            return Ok(request.clone());
        }
        let Some((store, owner)) = &self.native_history else {
            return Ok(request.clone());
        };
        let history = match owner {
            assistant_protocol::ConversationOwner::MainSession { session_id } => {
                store.load_conversation(session_id).await
            }
            assistant_protocol::ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                store
                    .load_child_conversation(session_id, child_task_id)
                    .await
            }
        }
        .map_err(|_| ModelError::Protocol("native request history is unavailable".into()))?;
        let mut prepared = request.clone();
        let mut instructions = request.system.parts().to_vec();
        if let Some(memory) = crate::context_compaction::memory::derive_pinned_memory_context(
            &history,
        )
        .map_err(|_| ModelError::Protocol("native request memory facts are invalid".into()))?
        {
            instructions.push(memory);
        }
        let mut capabilities = std::collections::BTreeMap::new();
        for message in &history.messages {
            if let agent_types::ConversationMessage::User(message) = message {
                for part in &message.parts {
                    if let agent_types::UserPart::InternalContext(part) = part
                        && matches!(
                            part.kind.as_str(),
                            "mcp_server_directory" | "mcp_server_selection" | "shell_environment"
                        )
                    {
                        capabilities.insert(part.kind.clone(), part.text.clone());
                    }
                }
            }
        }
        instructions.extend(capabilities.into_values());
        prepared.system = SystemPromptSnapshot::new(instructions);
        Ok(prepared)
    }

    async fn prepare_context_images(
        &self,
        request: &agent_model::ModelRequest,
        context: &mut agent_model::ModelCallContext,
    ) -> Result<(), ModelError> {
        if context.cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let Some(preprocessor) = &self.preprocessor else {
            return Ok(());
        };
        if context.prepared_images.is_empty() {
            let mut resources = Vec::new();
            for message in &request.conversation.messages {
                match message {
                    agent_types::ConversationMessage::User(message) => {
                        for part in &message.parts {
                            if let agent_types::UserPart::FileReferences(part) = part {
                                resources.extend(
                                    part.files
                                        .iter()
                                        .cloned()
                                        .map(ModelImageResource::FileReference),
                                );
                            }
                        }
                    }
                    agent_types::ConversationMessage::Tool(message) => {
                        for part in message.result.content.as_parts() {
                            if let agent_types::ToolResultPart::Image { image } = part {
                                resources.push(ModelImageResource::ToolImage {
                                    directory: self.tool_image_directory.clone(),
                                    reference: image.clone(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut seen = std::collections::BTreeSet::new();
            for resource in resources {
                let key = match &resource {
                    ModelImageResource::FileReference(reference) => {
                        (0_u8, reference.readable_path.clone())
                    }
                    ModelImageResource::LocalFile { path } => (2_u8, path.clone()),
                    ModelImageResource::ToolImage { reference, .. } => {
                        (1_u8, reference.relative_path().to_owned())
                    }
                };
                if !seen.insert(key) {
                    continue;
                }
                match preprocessor
                    .prepare(&resource, &context.cancellation)
                    .await?
                {
                    ModelImagePreparation::Image(image) => match resource {
                        ModelImageResource::FileReference(reference) => context
                            .prepared_images
                            .insert_file_reference(reference.readable_path, image),
                        ModelImageResource::LocalFile { path } => {
                            context.prepared_images.insert_file_reference(path, image)
                        }
                        ModelImageResource::ToolImage { reference, .. } => context
                            .prepared_images
                            .insert_tool_image(reference.relative_path().to_owned(), image),
                    },
                    ModelImagePreparation::NotImage => {}
                }
            }
        }
        Ok(())
    }
}

impl ModelService for RequestPreparingModelService {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn context_policy(&self) -> &agent_model::ContextPolicy {
        self.inner.context_policy()
    }

    fn count_input_tokens<'a>(
        &'a self,
        request: &'a agent_model::ModelRequest,
        context: &'a mut agent_model::ModelCallContext,
    ) -> agent_model::ModelInputTokenCountFuture<'a> {
        Box::pin(async move {
            if !self.input_token_count_supported {
                return if context.cancellation.is_cancelled() {
                    Err(ModelError::Cancelled)
                } else {
                    Ok(None)
                };
            }
            let history_before = self.measurement_history().await;
            let request = self.prepare_request(request).await?;
            self.prepare_context_images(&request, context).await?;
            let counted = self.inner.count_input_tokens(&request, context).await?;
            if let (Some(tokens), Some(before), Some(session), Some((_, owner))) = (
                counted,
                history_before,
                &self.measurement_session,
                &self.native_history,
            ) && !context.cancellation.is_cancelled()
                && self.measurement_history().await == Some(before)
                && let Ok(mut state) = session.lock_state()
                && state.server_history_key == self.session_binding
            {
                let key = match owner {
                    assistant_protocol::ConversationOwner::MainSession { .. } => String::new(),
                    assistant_protocol::ConversationOwner::ChildTask { child_task_id, .. } => {
                        child_task_id.as_str().to_owned()
                    }
                };
                state.context_measurements.insert(
                    key,
                    crate::session::ContextMeasurement {
                        history_hash: before,
                        session_binding: self.session_binding.clone(),
                        input_tokens: tokens,
                    },
                );
            }
            Ok(counted)
        })
    }

    fn compact(
        &self,
        request: agent_model::ModelRequest,
        mut context: agent_model::ModelCallContext,
    ) -> agent_model::ModelCompactionFuture<'_> {
        Box::pin(async move {
            let request = self.prepare_request(&request).await?;
            self.prepare_context_images(&request, &mut context).await?;
            self.inner.compact(request, context).await
        })
    }

    fn stream(
        &self,
        request: agent_model::ModelRequest,
        mut context: agent_model::ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            let request = self.prepare_request(&request).await?;
            self.prepare_context_images(&request, &mut context).await?;
            self.inner.stream(request, context).await
        })
    }
}

impl ModelService for ObservedModelService {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn context_policy(&self) -> &agent_model::ContextPolicy {
        self.inner.context_policy()
    }

    fn count_input_tokens<'a>(
        &'a self,
        request: &'a agent_model::ModelRequest,
        context: &'a mut agent_model::ModelCallContext,
    ) -> agent_model::ModelInputTokenCountFuture<'a> {
        self.inner.count_input_tokens(request, context)
    }

    fn compact(
        &self,
        request: agent_model::ModelRequest,
        context: agent_model::ModelCallContext,
    ) -> agent_model::ModelCompactionFuture<'_> {
        self.inner.compact(request, context)
    }

    fn stream(
        &self,
        request: agent_model::ModelRequest,
        context: agent_model::ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            let trace = context.trace.clone();
            self.observer.observe(ModelAttemptEvent::Started {
                trace: trace.clone(),
                attempt: 1,
            });
            match self.inner.stream(request, context).await {
                Ok(stream) => {
                    self.observer
                        .observe(ModelAttemptEvent::StreamEstablished { trace, attempt: 1 });
                    Ok(stream)
                }
                Err(error) => {
                    self.observer
                        .observe(ModelAttemptEvent::EstablishmentFailed {
                            trace,
                            attempt: 1,
                            error: error.clone(),
                            retry_reason: None,
                            will_retry: false,
                        });
                    Err(error)
                }
            }
        })
    }
}

/// 单次 Run 已同时冻结 Agent 规格和对应授权闸。
pub(super) struct CompiledRunAgent {
    history_model_binding: Option<Arc<OwnerModelBinding>>,
    shell: Option<crate::FrozenShellEnvironment>,
    prepared_model: crate::config::PreparedModel,
    auxiliary_model: Option<crate::config::PreparedModel>,
    auxiliary_selection: Option<assistant_protocol::ModelSelection>,
    agent: Agent,
    authorizer: Arc<dyn ToolAuthorizer>,
    compactor: Arc<RuntimeContextCompactor>,
    reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    goal_signal_latch: Option<Arc<GoalRunSignalLatch>>,
    skill_activation_latch: Arc<SkillActivationLatch>,
    can_speak: bool,
    disclosure_context: Option<agent_types::UserMessage>,
    resource_projection: super::resource_reference::ResourceReferenceProjection,
}

pub(super) struct CompiledRunParts {
    pub(super) history_model_binding: Option<Arc<OwnerModelBinding>>,
    pub(super) shell: Option<crate::FrozenShellEnvironment>,
    pub(super) model_binding: Arc<crate::config::PreparedModel>,
    pub(super) agent: Agent,
    pub(super) authorizer: Arc<dyn ToolAuthorizer>,
    pub(super) compactor: Arc<RuntimeContextCompactor>,
    pub(super) reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    pub(super) goal_signal_latch: Option<Arc<GoalRunSignalLatch>>,
    pub(super) skill_activation_latch: Arc<SkillActivationLatch>,
    pub(super) can_speak: bool,
    pub(super) disclosure_context: Option<agent_types::UserMessage>,
    pub(super) resource_projection: super::resource_reference::ResourceReferenceProjection,
}

impl CompiledRunAgent {
    /// 调用方取得配置接纳门禁后核验，直到 Run 领取完成前不得释放该门禁。
    pub(super) fn ensure_models_current(
        &self,
        registry: &crate::config::ConfigRegistry,
    ) -> RuntimeResult<()> {
        self.prepared_model.ensure_current(registry)?;
        if let Some(binding) = &self.history_model_binding {
            let state = binding.session.lock_state()?;
            if binding.owner
                != format!(
                    "{}:{}",
                    binding.session.id().as_str(),
                    state.server_history_key.as_deref().unwrap_or("initial")
                )
            {
                return Err(RuntimeError::ConfigurationConflict);
            }
        }
        if let Some(auxiliary) = &self.auxiliary_model {
            auxiliary.ensure_current(registry)?;
        }
        if registry.managed_models()?.settings.vision_model != self.auxiliary_selection {
            return Err(RuntimeError::ConfigurationConflict);
        }
        Ok(())
    }

    pub(super) fn into_parts(self) -> CompiledRunParts {
        CompiledRunParts {
            history_model_binding: self.history_model_binding,
            shell: self.shell,
            model_binding: Arc::new(self.prepared_model),
            agent: self.agent,
            authorizer: self.authorizer,
            compactor: self.compactor,
            reasoning_effort: self.reasoning_effort,
            goal_signal_latch: self.goal_signal_latch,
            skill_activation_latch: self.skill_activation_latch,
            can_speak: self.can_speak,
            disclosure_context: self.disclosure_context,
            resource_projection: self.resource_projection,
        }
    }
}

pub(super) struct RunAuthorizationInput {
    pub(super) permission_coordinator: Arc<PermissionCoordinator>,
    pub(super) approval_registry: Arc<ApprovalRegistry>,
    pub(super) variant: AgentVariant,
    pub(super) run_id: assistant_protocol::RunId,
    pub(super) cancellation: tokio_util::sync::CancellationToken,
    pub(super) events: ObservationCoordinator,
    pub(super) goal_binding: Option<GoalRunBinding>,
    pub(super) input_origin: crate::InputOrigin,
    pub(super) cross_session: Option<crate::CrossSessionInputEnvelope>,
}

/// 队列驱动与历史重入共同传入的 Run 装配资源；收敛参数数量并明确哪些能力来自 Runtime。
pub(super) struct RunCompilationResources<'a> {
    pub(super) attachments: Arc<
        std::sync::RwLock<
            std::collections::BTreeMap<assistant_protocol::AttachmentId, crate::StoredAttachment>,
        >,
    >,
    pub(super) shell: Option<crate::FrozenShellEnvironment>,
    pub(super) skill_catalog: crate::SkillCatalog,
    pub(super) model_factory: Arc<dyn crate::ModelServiceFactory>,
    pub(super) context_window: Arc<agent_sdk::ContextWindowEvaluator>,
    pub(super) run_tool_factory: &'a dyn RunToolFactory,
    pub(super) child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    pub(super) child_tasks: Arc<ChildTaskRegistry>,
    pub(super) store: Arc<dyn RuntimeStore>,
    pub(super) recall_reference_codec: Arc<crate::HmacRecallReferenceCodec>,
    pub(super) controller_tools: Arc<super::controller::ControllerToolCoordinator>,
    pub(super) output_dispatcher: Arc<dyn crate::ChannelOutputDispatcher>,
    pub(super) mcp_registry: Arc<McpRegistry>,
    pub(super) mcp_image_materializer: Arc<dyn McpImageMaterializer>,
}

/// 为新操作读取最新固定模型参数，不隐式联网；活动 Run 继续持有自身冻结绑定。
/// 调用方负责在既有 Session／配置门禁内接纳结果；本函数不修改 Session 或 Store。
pub(super) async fn resolve_session_model(
    registry: &crate::config::ConfigRegistry,
    session: &SessionController,
    selection: Option<&assistant_protocol::ModelSelection>,
    store: &dyn RuntimeStore,
) -> RuntimeResult<Arc<crate::config::PreparedModel>> {
    // 固定配置更新优先；没有固定记录的在线模型仍可复用本进程已接纳的参数。
    let cached = session.lock_state()?.model_binding.clone();
    let effective = registry.effective_model_selection(selection)?;
    if let Some(cached) = cached
        && effective.as_ref() == Some(&cached.selection)
        && cached.ensure_current(registry).is_ok()
        && registry
            .fixed_model(cached.selection.clone(), store)
            .await?
            .is_none()
    {
        return Ok(cached);
    }
    let snapshot = registry.snapshot()?;
    let prepared = Arc::new(
        registry
            .configured_model(&snapshot, selection, store)
            .await?,
    );
    Ok(prepared)
}

impl AssistantRuntime {
    /// 为独立手动压缩冻结当前 Session 的模型服务；不装配 Agent 或工具。
    pub(super) async fn compile_session_compactor(
        &self,
        session: &Arc<SessionController>,
        selection: Option<&assistant_protocol::ModelSelection>,
    ) -> RuntimeResult<(RuntimeContextCompactor, Arc<crate::config::PreparedModel>)> {
        self.model_factory.ensure_available()?;
        let snapshot = self.config_registry.snapshot()?;
        let prepared = resolve_session_model(
            &self.config_registry,
            session,
            selection,
            self.store.as_ref(),
        )
        .await?;
        let (state_owner, generation) = {
            let state = session.lock_state()?;
            (
                format!(
                    "{}:{}",
                    session.id().as_str(),
                    state.server_history_key.as_deref().unwrap_or("initial")
                ),
                state.body_generation,
            )
        };
        let mut compiled = compile_resolved_model_service_with_history(
            &snapshot,
            &prepared.model,
            self.model_factory.as_ref(),
            None,
            Some(&format!("{state_owner}:{generation}")),
            Some(&state_owner),
            &conversation_routing_id(&assistant_protocol::ConversationOwner::MainSession {
                session_id: session.id().clone(),
            }),
        )?;
        bind_image_preparation(
            &mut compiled,
            session.environment(),
            Some((
                self.store.clone(),
                assistant_protocol::ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                },
            )),
            Some(session.clone()),
        );
        let (reasoning, provider_options) = protocol_request_options(
            &compiled.provider,
            compiled.protocol,
            &compiled.capabilities,
            session.reasoning_effort()?,
        )?;
        let request = agent_core::ModelRequestConfig {
            tool_choice: ToolChoice::None,
            generation: prepared.model.generation().clone(),
            reasoning,
            provider_options,
        };
        let (variant, shell) = {
            let state = session.lock_state()?;
            (state.current_variant, state.agent_shell_environment.clone())
        };
        let disclosure = McpRunDisclosure::compile(
            self.mcp_service.registry.as_ref(),
            self.permission_coordinator.as_ref(),
            &session.permission_scopes(),
            variant,
            None,
        )?;
        let mcp = {
            let state = session.lock_state()?;
            let journal = state
                .journal
                .as_ref()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "manual compaction MCP context",
                })?;
            disclosure.context_for_compaction(&journal.snapshot())?
        };
        let shell_context = shell
            .as_ref()
            .map(|shell| shell.context_message(&session.environment().working_directory))
            .transpose()?;
        Ok((
            RuntimeContextCompactor::for_manual(
                compiled.model,
                session.current_system_prompt()?,
                request,
            )
            .with_restored_context(mcp, shell_context),
            prepared,
        ))
    }
}

pub(super) async fn compile_run_agent(
    session: Arc<SessionController>,
    snapshot: &ConfigSnapshot,
    registry: &crate::config::ConfigRegistry,
    resources: RunCompilationResources<'_>,
    authorization: RunAuthorizationInput,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledRunAgent> {
    resources.model_factory.ensure_available()?;
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let selection = session.model_selection()?;
    let prepared = registry
        .prepare_model(snapshot, selection.as_ref(), resources.store.as_ref())
        .await?;
    let model_config = &prepared.model;
    let history_owner = {
        let state = session.lock_state()?;
        format!(
            "{}:{}",
            session.id().as_str(),
            state.server_history_key.as_deref().unwrap_or("initial")
        )
    };
    let mut compiled = compile_resolved_model_service_with_history(
        snapshot,
        model_config,
        resources.model_factory.as_ref(),
        model_attempt_observer.clone(),
        Some(&format!(
            "{history_owner}:{}",
            session.lock_state()?.body_generation
        )),
        Some(&history_owner),
        &conversation_routing_id(&assistant_protocol::ConversationOwner::MainSession {
            session_id: session.id().clone(),
        }),
    )?;
    let auxiliary_selection = registry.managed_models()?.settings.vision_model.clone();
    let mut auxiliary_prepared = None;
    let mut auxiliary_binding = None;
    let image_inspector = if !compiled.capabilities.image_input && compiled.capabilities.tool_calls
    {
        if let Some(selection) = &auxiliary_selection {
            let prepared_auxiliary = registry
                .prepare_model(snapshot, Some(selection), resources.store.as_ref())
                .await?;
            let binding = Arc::new(AuxiliaryVisionBinding {
                snapshot: Arc::new(snapshot.clone()),
                model: prepared_auxiliary.model.clone(),
                factory: resources.model_factory.clone(),
                environment: session.environment().clone(),
                selection: selection.clone(),
                timeout: active
                    .vision()
                    .map_or(Duration::from_secs(60), |vision| vision.timeout),
                output_budget: active
                    .vision()
                    .map_or(4096, |vision| vision.max_output_tokens),
            });
            let inspector =
                binding.compile(&assistant_protocol::ConversationOwner::MainSession {
                    session_id: session.id().clone(),
                })?;
            auxiliary_binding = Some(binding);
            auxiliary_prepared = Some(prepared_auxiliary);
            Some(inspector)
        } else {
            None
        }
    } else {
        None
    };
    let history_model_binding = Some({
        Arc::new(OwnerModelBinding {
            snapshot: Arc::new(snapshot.clone()),
            model: model_config.clone(),
            factory: resources.model_factory.clone(),
            environment: session.environment().clone(),
            session: session.clone(),
            owner: history_owner.clone(),
            store: resources.store.clone(),
            observer: model_attempt_observer.clone(),
            auxiliary: auxiliary_binding,
        })
    });
    prepared.ensure_current(registry)?;
    if let Some(auxiliary) = &auxiliary_prepared {
        auxiliary.ensure_current(registry)?;
    }
    let current_prompt = session.current_system_prompt()?;
    let resource_projection =
        {
            let attachments = resources.attachments.read().map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "attachment registry",
                }
            })?;
            super::resource_reference::ResourceReferenceProjection::new(
                resources.store.as_ref(),
                attachments
                    .values()
                    .filter(|a| a.session_id == *session.id()),
                session.environment(),
                &current_prompt,
            )?
        };
    bind_image_preparation(
        &mut compiled,
        session.environment(),
        Some((
            resources.store.clone(),
            assistant_protocol::ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
        )),
        Some(session.clone()),
    );
    let requested_effort = session.reasoning_effort()?;
    let frozen_reasoning_effort = requested_effort.or_else(|| {
        compiled
            .capabilities
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.default_effort.map(protocol_effort_key))
    });
    let (reasoning, provider_options) = protocol_request_options(
        &compiled.provider,
        compiled.protocol,
        &compiled.capabilities,
        requested_effort,
    )?;
    // Conversation Recall 同时具备检索和稳定引用续读能力，但两个 trait 保持独立，避免将
    // 有序续读语义强加给所有通用 Recall Source。
    let conversation_recall = Arc::new(crate::conversation_recall::RuntimeConversationRecall::new(
        resources.store.clone(),
        resources.recall_reference_codec.clone(),
        session.id().clone(),
        session.environment().workspace_id.clone(),
    ));
    let has_auxiliary_vision = image_inspector.is_some();
    let bundle = resources
        .run_tool_factory
        .compile(crate::RunToolFactoryRequest {
            session_id: session.id(),
            environment: session.environment(),
            shell: resources.shell.as_ref(),
            pinned_memory: Arc::new(crate::RuntimePinnedMemoryStore::new(
                resources.store.clone(),
                session.id().clone(),
            )),
            conversation_recall: conversation_recall.clone(),
            conversation_recall_reader: conversation_recall,
            image_inspector,
            read_image_enabled: compiled.capabilities.image_input
                && compiled.capabilities.tool_calls
                && compiled.capabilities.tool_image_projection
                    != agent_model::ToolImageProjection::Unsupported,
        })
        .map_err(|source| {
            if source.kind() == RunToolFactoryErrorKind::WorkingDirectoryUnavailable
                && let Some(workspace_id) = session.environment().workspace_id.clone()
            {
                return RuntimeError::WorkspaceUnavailable { workspace_id };
            }
            RuntimeError::RunToolsBuildFailed { source }
        })?;
    let default_rules = bundle.default_rules();
    let artifact_files = bundle.artifact_files();
    let (base_tools, infrastructure_policies) = bundle.into_parts();
    let active_skill_names = {
        let state = session.lock_state()?;
        let mut latest_names = std::collections::BTreeSet::new();
        state
            .skill_activations
            .iter()
            .rev()
            .filter(|activation| {
                matches!(
                    &activation.owner,
                    SkillActivationOwner::Session(owner) if owner == session.id()
                )
            })
            // 同名只比较最后一次激活，文件回退旧版本时也必须重新加载。
            .filter(|activation| latest_names.insert(activation.name.clone()))
            .filter(|activation| {
                resources
                    .skill_catalog
                    .definitions
                    .iter()
                    .any(|definition| {
                        definition.name == activation.name
                            && definition.definition_digest == activation.definition_digest
                    })
            })
            .map(|activation| activation.name.clone())
            .collect::<Vec<_>>()
    };
    let skill_activation_latch = Arc::new(SkillActivationLatch::new(active_skill_names));
    let goal_signal_latch = if let Some(binding) = authorization.goal_binding.as_ref() {
        let state = session.lock_state()?;
        let goal = state
            .goal
            .as_ref()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "goal run binding",
            })?;
        if goal.id != binding.goal_id
            || goal.generation != binding.generation
            || binding.run_id != authorization.run_id
            || !matches!(goal.state, GoalState::Running)
        {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "goal run binding",
            });
        }
        Some(Arc::new(GoalRunSignalLatch::new(binding.clone())))
    } else {
        None
    };
    let session_id = session.id().clone();
    let run_id = authorization.run_id.clone();
    let permission_scopes = session.permission_scopes();
    let (input_selection, goal_selection) = {
        let state = session.lock_state()?;
        let input_id = state
            .runs
            .get(&authorization.run_id)
            .map(|run| run.input_id().clone());
        let input_selection = input_id.as_ref().and_then(|input_id| {
            state
                .mcp_selections
                .iter()
                .find(|selection| selection.input_id.as_ref() == Some(input_id))
                .cloned()
        });
        let goal_selection = authorization.goal_binding.as_ref().and_then(|_| {
            state.goal.as_ref().and_then(|goal| {
                goal.mcp_server_key.as_ref().map(|server_key| {
                    (
                        server_key.clone(),
                        goal.objective.source_message_id.clone(),
                        goal.created_at_ms,
                    )
                })
            })
        });
        (input_selection, goal_selection)
    };
    let mcp_selection = match (input_selection, goal_selection) {
        (Some(selection), _) => Some(selection),
        (None, Some((server_key, message_id, created_at_ms))) => {
            let display_name = resources
                .mcp_registry
                .catalog_server(&server_key)?
                .map_or_else(
                    || server_key.as_str().to_owned(),
                    |server| server.display_name,
                );
            Some(crate::StoredMcpSelection {
                selection_id: "goal-mcp-selection".to_owned(),
                session_id: session.id().clone(),
                input_id: None,
                message_id,
                server_key,
                display_name,
                created_at_ms,
            })
        }
        (None, None) => None,
    };
    let mcp_disclosure = McpRunDisclosure::compile(
        resources.mcp_registry.as_ref(),
        authorization.permission_coordinator.as_ref(),
        &permission_scopes,
        authorization.variant,
        mcp_selection.as_ref(),
    )?;
    let shell_context = resources
        .shell
        .as_ref()
        .map(|shell| shell.context_message(&session.environment().working_directory))
        .transpose()?;
    let restored_mcp_context = {
        let state = session.lock_state()?;
        let journal = state
            .journal
            .as_ref()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "Run compaction MCP context",
            })?;
        mcp_disclosure.context_for_compaction(&journal.snapshot())?
    };
    let parent_compactor = Arc::new(
        RuntimeContextCompactor::for_parent(
            compiled.background_model.clone(),
            session.current_system_prompt()?,
        )
        .with_restored_context(restored_mcp_context.clone(), shell_context.clone()),
    );
    let mcp_scope = mcp_disclosure.scope.clone();
    let authorizer = Arc::new(
        RuntimeToolAuthorizer::new(
            RunAuthorizationScope {
                variant: authorization.variant,
            },
            session.clone(),
            permission_scopes.clone(),
            authorization.permission_coordinator.clone(),
            infrastructure_policies.clone(),
            session.environment(),
            Arc::new(RuntimeApprovalResolver {
                registry: authorization.approval_registry.clone(),
                session_id,
                run_id,
                child_task_id: None,
                variant: authorization.variant,
                workspace_id: session.environment().workspace_id.clone(),
                cancellation: authorization.cancellation.clone(),
                events: authorization.events.clone(),
            }),
        )?
        .with_default_rules(default_rules.clone())
        .with_goal_signal_latch(goal_signal_latch.clone())
        .with_mcp_registry(resources.mcp_registry.clone()),
    );

    let model_request = agent_core::ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: model_config.generation().clone(),
        reasoning,
        provider_options,
    };
    // 不具备 Tool Call 能力的模型维持历史纯文本路径。父 Agent 在这里加入 Run 级
    // Runtime 工具；child 保留 Base ToolSet，并在具体 child execution 创建后追加绑定
    // 独立 ActivationLatch 的 load_skill，避免 sibling 共享激活状态。
    let can_speak = compiled.model.capabilities().tool_calls
        && session.role()? == crate::SessionRole::Controller;
    let mut tool_assembly = RunToolAssembly::default();
    tool_assembly.contribute(RunToolContribution::frozen(base_tools.clone()));
    // child 的辅助识图在获得独立 owner 后加入，不能复用父会话绑定的工具句柄。
    let mut child_base_tools = if has_auxiliary_vision {
        base_tools.clone().without_tool("inspect_images")
    } else {
        base_tools.clone()
    };
    if compiled.model.capabilities().tool_calls {
        tool_assembly.contribute(
            RunToolContribution::tool(crate::feedback::AskUserTool::new(
                session.clone(),
                authorization.run_id.clone(),
                resources.store.clone(),
                authorization.events.clone(),
            ))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "feedback tool definition",
            })?,
        );
        if let Some(files) = artifact_files {
            tool_assembly.contribute(
                RunToolContribution::tool(crate::artifact::DeclareArtifactTool::new(
                    files,
                    authorization.run_id.clone(),
                ))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "artifact tool definition",
                })?,
            );
        }
        if !mcp_scope.is_empty() {
            let image_directory = (compiled.capabilities.tool_image_projection
                != agent_model::ToolImageProjection::Unsupported)
                .then(|| session.environment().session_tool_image_directory.clone());
            let mut mcp_tools = RunToolAssembly::default();
            mcp_tools.contribute(
                RunToolContribution::tool(DiscoverMcpTools::new(
                    resources.mcp_registry.clone(),
                    mcp_scope.clone(),
                    authorization.permission_coordinator.clone(),
                    permission_scopes.clone(),
                    authorization.variant,
                ))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "MCP discovery tool definition",
                })?,
            );
            mcp_tools.contribute(
                RunToolContribution::tool(CallMcpTool::new(
                    resources.mcp_registry.clone(),
                    mcp_scope.clone(),
                    image_directory,
                    resources.mcp_image_materializer.clone(),
                ))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "MCP call tool definition",
                })?,
            );
            let mcp_tools =
                mcp_tools
                    .freeze()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "MCP gateway tool assembly",
                    })?;
            child_base_tools = child_base_tools.try_merge(mcp_tools.clone()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "child MCP tool assembly",
                }
            })?;
            tool_assembly.contribute(RunToolContribution::frozen(mcp_tools));
        }
        if session.role()? == crate::SessionRole::Controller {
            tool_assembly.contribute(
                RunToolContribution::tool(SpeakTool::new(
                    session.clone(),
                    authorization.run_id.clone(),
                    resources.output_dispatcher.clone(),
                ))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "speak tool definition",
                })?,
            );
        }
        tool_assembly.contribute(
            RunToolContribution::tool(LoadSkillTool::new(
                resources.skill_catalog.clone(),
                skill_activation_latch.clone(),
            ))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "load skill tool definition",
            })?,
        );
        tool_assembly.contribute(
            RunToolContribution::tool(UpdatePlanTool::new(
                session.clone(),
                resources.store.clone(),
                authorization.events.clone(),
            ))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "update plan tool definition",
            })?,
        );
        tool_assembly.contribute(
            RunToolContribution::tool(UpdateGoalTool::new(goal_signal_latch.clone())).map_err(
                |_| RuntimeError::InternalStateUnavailable {
                    component: "update goal tool definition",
                },
            )?,
        );
        let controller_tools = if session.role()? == crate::SessionRole::Controller
            && authorization.input_origin == crate::InputOrigin::User
            && authorization.cross_session.is_none()
        {
            Some(
                super::controller::controller_tool_set(
                    resources.controller_tools.clone(),
                    session.id().clone(),
                    authorization.run_id.clone(),
                )
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "controller tool definitions",
                })?,
            )
        } else {
            None
        };
        let delegation = active.delegation();
        let mut child_generation = model_request.generation.clone();
        child_generation.max_output_tokens = Some(
            child_generation
                .max_output_tokens
                .unwrap_or(u32::MAX)
                .min(delegation.max_output_tokens().get()),
        );
        let child_budget = ExecutionBudget {
            max_steps: Some(
                active
                    .budget()
                    .max_steps
                    .unwrap_or(u32::MAX)
                    .min(delegation.max_steps().get()),
            ),
            max_tool_calls: Some(
                active
                    .budget()
                    .max_tool_calls
                    .unwrap_or(u32::MAX)
                    .min(delegation.max_tool_calls().get()),
            ),
        };
        let mut child_prompt_parts = session.current_system_prompt()?.parts().to_vec();
        child_prompt_parts.push(crate::delegation::CHILD_AGENT_INSTRUCTION_V1.to_owned());
        let child_prompt = SystemPromptSnapshot::new(child_prompt_parts);
        let child_compactor = Arc::new(
            RuntimeContextCompactor::for_child(
                compiled.background_model.clone(),
                child_prompt.clone(),
            )
            .with_restored_context(mcp_disclosure.context.clone(), shell_context.clone()),
        );
        let mut child_builder = AgentBuilder::new(
            compiled.background_model.clone(),
            child_prompt,
            resources.context_window.clone(),
        )
        .tools(child_base_tools)
        .model_request(agent_core::ModelRequestConfig {
            tool_choice: ToolChoice::Auto,
            generation: child_generation,
            reasoning: model_request.reasoning.clone(),
            provider_options: model_request.provider_options.clone(),
        })
        .budget(child_budget);
        if active.guardrails().repeated_invocation.is_some()
            || active.guardrails().consecutive_failures.is_some()
        {
            child_builder = child_builder.guardrails(active.guardrails().clone());
        }
        let child_agent = Arc::new(
            child_builder
                .build()
                .map_err(|source| RuntimeError::AgentBuildFailed { source })?,
        );
        let delegation_controller =
            Arc::new(ParentDelegationController::new(ParentDelegationResources {
                context_management_mode: model_config.context_management_mode(),
                context_window_tokens: compiled.model.context_window_tokens(),
                session: session.clone(),
                parent_run_id: authorization.run_id,
                variant: authorization.variant,
                child_agent,
                child_model_binding: history_model_binding.clone(),
                child_compactor,
                store: resources.store,
                registry: resources.child_tasks,
                workspace_factory: resources.child_task_workspace_factory,
                permission_coordinator: authorization.permission_coordinator,
                approval_registry: authorization.approval_registry,
                infrastructure_policies,
                default_rules,
                events: authorization.events,
                limits: delegation,
                skill_catalog: resources.skill_catalog.clone(),
                mcp_registry: resources.mcp_registry.clone(),
                disclosure_context: mcp_disclosure.context.clone(),
                resource_projection: resource_projection.for_child(),
                shell_context,
            }));
        tool_assembly.contribute(
            RunToolContribution::tool(DelegateTaskTool::new(delegation_controller)).map_err(
                |_| RuntimeError::InternalStateUnavailable {
                    component: "delegate task tool definition",
                },
            )?,
        );
        if let Some(controller_tools) = controller_tools {
            tool_assembly.contribute(RunToolContribution::frozen(controller_tools));
        }
    }
    let parent_tools =
        tool_assembly
            .freeze()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "run tool assembly",
            })?;

    let mut builder = AgentBuilder::new(
        compiled.model,
        session.current_system_prompt()?,
        resources.context_window,
    )
    .tools(parent_tools)
    .model_request(model_request)
    .budget(active.budget().clone());
    if active.guardrails().repeated_invocation.is_some()
        || active.guardrails().consecutive_failures.is_some()
    {
        builder = builder.guardrails(active.guardrails().clone());
    }
    let agent = builder
        .build()
        .map_err(|source| RuntimeError::AgentBuildFailed { source })?;
    let result = CompiledRunAgent {
        history_model_binding,
        shell: resources.shell,
        prepared_model: prepared,
        auxiliary_model: auxiliary_prepared,
        auxiliary_selection,
        agent,
        authorizer,
        compactor: parent_compactor,
        reasoning_effort: frozen_reasoning_effort,
        goal_signal_latch,
        skill_activation_latch,
        can_speak,
        disclosure_context: mcp_disclosure.context,
        resource_projection,
    };
    result.ensure_models_current(registry)?;
    Ok(result)
}

pub(super) fn compile_resolved_model_service(
    snapshot: &ConfigSnapshot,
    model_config: &ResolvedModelConfig,
    model_factory: &dyn crate::ModelServiceFactory,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
    routing_id: &str,
) -> RuntimeResult<CompiledModelService> {
    compile_resolved_model_service_with_history(
        snapshot,
        model_config,
        model_factory,
        model_attempt_observer,
        None,
        None,
        routing_id,
    )
}

/// 主会话服务只读取当前可靠历史；旁路服务单独按同一配置装配为无状态，
/// 不允许摘要、标题和辅助识图创建或消费业务续接凭据。
fn compile_resolved_model_service_with_history(
    snapshot: &ConfigSnapshot,
    model_config: &ResolvedModelConfig,
    model_factory: &dyn crate::ModelServiceFactory,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
    history_owner: Option<&str>,
    state_owner: Option<&str>,
    routing_id: &str,
) -> RuntimeResult<CompiledModelService> {
    model_factory.ensure_available()?;
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let transport = active.transport();
    let state_binding = state_owner
        .or(Some("unbound"))
        .zip(model_config.server_history_key.as_deref())
        .map(|(owner, key)| format!("{owner}:{key}"));
    let history_binding = history_owner
        .filter(|_| model_config.server_history_enabled)
        .zip(model_config.server_history_key.as_deref())
        .map(|(owner, key)| format!("{owner}:{key}"));
    let create_bundle = |binding: Option<&str>| {
        model_factory
            .create_model(ModelServiceFactoryRequest {
                conversation_routing_id: Some(routing_id),
                context_management_mode: model_config.context_management_mode(),
                server_history_binding: binding,
                provider_state_binding: state_binding.as_deref(),
                input_token_count_supported: model_config.input_token_count_supported,
                compaction_threshold_tokens: std::num::NonZeroU64::new(
                    (model_config.context_window_tokens() as f64 * super::CONTEXT_WINDOW_THRESHOLD)
                        .floor() as u64,
                )
                .unwrap_or(std::num::NonZeroU64::MIN),
                external_configuration: model_config.external_configuration.as_ref(),
                provider: model_config.provider(),
                protocol: model_config.protocol(),
                capabilities: model_config.capabilities(),
                endpoint: model_config.endpoint(),
                model: model_config.model(),
                api_key: model_config.api_key(),
                context_window_tokens: model_config.context_window_tokens(),
                max_input_tokens: model_config.max_input_tokens(),
                connect_timeout: transport.connect_timeout(),
                request_timeout: transport.request_timeout(),
            })
            .map_err(|source| RuntimeError::ModelBuildFailed { source })
    };
    let bundle = create_bundle(history_binding.as_deref())?;
    let background_base = if history_binding.is_some() {
        create_bundle(None)?.model
    } else {
        bundle.model.clone()
    };
    // 配置选择机制与真实装配必须一致；未接入原生 compact 的 Adapter 不得静默走本地。
    if bundle.model.context_policy().mode() != model_config.context_management_mode()
        || background_base.context_policy().mode() != model_config.context_management_mode()
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "当前模型装配尚不支持所选压缩机制。",
        });
    }
    let base_model = bundle.model;
    let image_preprocessor = bundle.image_preprocessor;
    let background_model: Arc<dyn ModelService> = match active.retry_policy() {
        Some(policy) => Arc::new(RetryingModelService::new(
            background_base.clone(),
            policy.clone(),
        )),
        None => background_base,
    };
    let model: Arc<dyn ModelService> = if history_binding.is_some() {
        Arc::new(CallScopedHistoryModelService::new(
            base_model,
            active.retry_policy().cloned(),
            model_attempt_observer,
        ))
    } else {
        match (active.retry_policy(), model_attempt_observer) {
            (Some(policy), Some(observer)) => Arc::new(RetryingModelService::with_observer(
                base_model,
                policy.clone(),
                observer,
            )) as Arc<dyn ModelService>,
            (Some(policy), None) => Arc::new(RetryingModelService::new(base_model, policy.clone()))
                as Arc<dyn ModelService>,
            (None, Some(observer)) => Arc::new(ObservedModelService {
                inner: base_model,
                observer,
            }) as Arc<dyn ModelService>,
            (None, None) => base_model,
        }
    };
    Ok(CompiledModelService {
        input_token_count_supported: model_config.input_token_count_supported,
        model,
        background_model,
        provider: model_config.provider().clone(),
        protocol: model_config.protocol(),
        capabilities: model_config.capabilities().clone(),
        max_output_tokens: model_config.max_output_tokens(),
        request_timeout: transport.request_timeout(),
        image_preprocessor,
    })
}

fn bind_image_preparation(
    compiled: &mut CompiledModelService,
    environment: &crate::SessionExecutionEnvironment,
    native_history: Option<(Arc<dyn RuntimeStore>, assistant_protocol::ConversationOwner)>,
    measurement_session: Option<Arc<SessionController>>,
) {
    let preprocessor = compiled
        .image_preprocessor
        .take()
        .filter(|_| compiled.capabilities.image_input);
    let session_binding = measurement_session.as_ref().and_then(|session| {
        session
            .lock_state()
            .ok()
            .and_then(|state| state.server_history_key.clone())
    });
    if preprocessor.is_none() && native_history.is_none() {
        return;
    }
    compiled.model = Arc::new(RequestPreparingModelService {
        inner: compiled.model.clone(),
        preprocessor: preprocessor.clone(),
        tool_image_directory: environment.session_tool_image_directory.clone(),
        native_history: native_history.clone(),
        measurement_session: measurement_session.clone(),
        session_binding: session_binding.clone(),
        input_token_count_supported: compiled.input_token_count_supported,
    });
    compiled.background_model = Arc::new(RequestPreparingModelService {
        inner: compiled.background_model.clone(),
        preprocessor,
        tool_image_directory: environment.session_tool_image_directory.clone(),
        native_history,
        measurement_session,
        session_binding,
        input_token_count_supported: compiled.input_token_count_supported,
    });
}

/// 按已编译 Route、Protocol 和 capability 编译业务请求所需的 reasoning 选项。
pub(super) fn protocol_request_options(
    provider: &agent_types::ProviderId,
    protocol: ModelProtocol,
    capabilities: &ResolvedModelCapabilities,
    requested_effort: Option<assistant_protocol::ReasoningEffortKey>,
) -> RuntimeResult<(Option<ReasoningConfig>, ProviderOptions)> {
    let mut provider_options = ProviderOptions::new();
    if protocol == ModelProtocol::OpenAiChatCompletions && capabilities.reasoning_enabled() {
        let options = match provider.as_str() {
            "deepseek" | "zhipu" => Some(serde_json::json!({"thinking": {"type": "enabled"}})),
            "moonshot"
                if capabilities.reasoning.as_ref().is_some_and(|reasoning| {
                    reasoning.mode == assistant_protocol::ModelReasoningMode::Optional
                }) =>
            {
                Some(serde_json::json!({"thinking": {"type": "enabled"}}))
            }
            "dashscope_api" | "dashscope_plan" => {
                Some(serde_json::json!({"enable_thinking": true, "preserve_thinking": true}))
            }
            _ => None,
        };
        if let Some(options) = options {
            provider_options
                .insert(provider.as_str(), options)
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "provider reasoning options",
                })?;
        }
    }
    let effective = requested_effort.or_else(|| {
        capabilities
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.default_effort.map(protocol_effort_key))
    });
    let reasoning = capabilities.reasoning_enabled().then(|| ReasoningConfig {
        effort: effective.map(model_effort_key),
    });
    Ok((reasoning, provider_options))
}

fn protocol_effort_key(value: crate::ReasoningEffortKey) -> assistant_protocol::ReasoningEffortKey {
    match value {
        crate::ReasoningEffortKey::Low => assistant_protocol::ReasoningEffortKey::Low,
        crate::ReasoningEffortKey::Medium => assistant_protocol::ReasoningEffortKey::Medium,
        crate::ReasoningEffortKey::High => assistant_protocol::ReasoningEffortKey::High,
        crate::ReasoningEffortKey::XHigh => assistant_protocol::ReasoningEffortKey::XHigh,
        crate::ReasoningEffortKey::Max => assistant_protocol::ReasoningEffortKey::Max,
    }
}

fn model_effort_key(value: assistant_protocol::ReasoningEffortKey) -> agent_model::ReasoningEffort {
    match value {
        assistant_protocol::ReasoningEffortKey::Low => agent_model::ReasoningEffort::Low,
        assistant_protocol::ReasoningEffortKey::Medium => agent_model::ReasoningEffort::Medium,
        assistant_protocol::ReasoningEffortKey::High => agent_model::ReasoningEffort::High,
        assistant_protocol::ReasoningEffortKey::XHigh => agent_model::ReasoningEffort::XHigh,
        assistant_protocol::ReasoningEffortKey::Max => agent_model::ReasoningEffort::Max,
    }
}

#[cfg(test)]
mod tests {
    use agent_model::{
        GenerationConfig, ModelCallContext, ModelCapabilities, ModelImagePreparationFuture,
        ModelRequest, PreparedModelImage,
    };
    use agent_types::{
        ConversationMessage, ConversationSnapshot, FileReference, FileReferencesPart, MessageId,
        PartId, TranscriptVisibility, UserMessage, UserMessageOrigin, UserPart,
    };

    use super::*;

    struct AlwaysImagePreprocessor;

    impl ModelImagePreprocessor for AlwaysImagePreprocessor {
        fn prepare<'a>(
            &'a self,
            _resource: &'a ModelImageResource,
            _cancellation: &'a tokio_util::sync::CancellationToken,
        ) -> ModelImagePreparationFuture<'a> {
            Box::pin(async {
                Ok(ModelImagePreparation::Image(PreparedModelImage {
                    media_type: "image/jpeg".to_owned(),
                    bytes: Arc::from([1_u8]),
                }))
            })
        }
    }

    #[derive(Default)]
    struct ProviderReachedModel {
        capabilities: ModelCapabilities,
    }

    impl ModelService for ProviderReachedModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            8_192
        }

        fn count_input_tokens<'a>(
            &'a self,
            _: &'a ModelRequest,
            context: &'a mut ModelCallContext,
        ) -> agent_model::ModelInputTokenCountFuture<'a> {
            Box::pin(async move {
                assert_eq!(context.prepared_images.len(), 11);
                Ok(Some(11))
            })
        }

        fn stream(
            &self,
            _request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(async move {
                assert_eq!(context.prepared_images.len(), 11);
                Err(ModelError::Provider {
                    message: "provider image limit".to_owned(),
                    status: Some(400),
                })
            })
        }
    }

    #[tokio::test]
    async fn image_preparation_does_not_impose_a_global_image_count_limit() {
        let files = (0..11)
            .map(|index| FileReference {
                original_name: format!("image-{index}.jpg"),
                readable_path: format!("attachments/image-{index}.jpg"),
            })
            .collect();
        let request = ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
                id: MessageId::new("user-images").expect("message id"),
                origin: UserMessageOrigin::User,
                transcript_visibility: TranscriptVisibility::Visible,
                parts: vec![UserPart::FileReferences(FileReferencesPart {
                    id: PartId::new("user-images-files").expect("part id"),
                    files,
                })],
            })]),
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        };
        let service = RequestPreparingModelService {
            inner: Arc::new(ProviderReachedModel::default()),
            preprocessor: Some(Arc::new(AlwaysImagePreprocessor)),
            native_history: None,
            measurement_session: None,
            session_binding: None,
            input_token_count_supported: true,
            tool_image_directory: String::new(),
        };

        let mut context = ModelCallContext::default();
        assert_eq!(
            service
                .count_input_tokens(&request, &mut context)
                .await
                .unwrap(),
            Some(11)
        );
        assert_eq!(context.prepared_images.len(), 11);
        let result = service.stream(request, context).await;

        assert!(matches!(
            result,
            Err(ModelError::Provider { message, status: Some(400) })
                if message == "provider image limit"
        ));
    }

    struct HistoryScript {
        capabilities: ModelCapabilities,
        replies: std::sync::Mutex<
            std::collections::VecDeque<Result<Vec<agent_model::ModelEvent>, ModelError>>,
        >,
        calls: std::sync::Mutex<Vec<(ModelRequest, ModelCallContext)>>,
        cancel_on_rejection: bool,
    }
    impl ModelService for HistoryScript {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }
        fn context_window_tokens(&self) -> u64 {
            8192
        }
        fn stream(
            &self,
            request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((request, context.clone()));
                let reply = self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("script reply");
                if self.cancel_on_rejection && matches!(reply, Err(ModelError::HistoryUnavailable))
                {
                    context.cancellation.cancel();
                }
                reply.map(|events| {
                    Box::pin(futures_util::stream::iter(events)) as agent_model::ModelEventStream
                })
            })
        }
    }
    fn history_script(
        replies: Vec<Result<Vec<agent_model::ModelEvent>, ModelError>>,
        cancel_on_rejection: bool,
    ) -> Arc<HistoryScript> {
        Arc::new(HistoryScript {
            capabilities: Default::default(),
            replies: std::sync::Mutex::new(replies.into()),
            calls: Default::default(),
            cancel_on_rejection,
        })
    }
    fn history_request() -> ModelRequest {
        ModelRequest {
            system: SystemPromptSnapshot::new(vec!["frozen prompt".into()]),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::None,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }
    fn history_finished() -> Vec<agent_model::ModelEvent> {
        let message = agent_types::AssistantMessage {
            purpose: agent_types::AssistantMessagePurpose::ModelResponse,
            id: MessageId::new("history-final").unwrap(),
            model: agent_types::ModelIdentity::new(
                agent_types::ProviderId::new("fixture").unwrap(),
                "fixture",
            ),
            parts: vec![],
            finish_reason: agent_types::FinishReason::Stop,
            usage: None,
        };
        vec![
            agent_model::ModelEvent::TurnStarted {
                message_id: message.id.clone(),
                model: message.model.clone(),
            },
            agent_model::ModelEvent::TurnFinished { message },
        ]
    }
    fn history_interrupted() -> Vec<agent_model::ModelEvent> {
        vec![agent_model::ModelEvent::TurnFailed {
            error: ModelError::Transport {
                kind: agent_model::ModelTransportErrorKind::Interrupted,
                message: "disconnected".into(),
            },
        }]
    }
    fn call_scoped_history(inner: Arc<dyn ModelService>) -> CallScopedHistoryModelService {
        CallScopedHistoryModelService::new(
            inner,
            Some(agent_model::ModelRetryPolicy::new(
                [agent_model::ModelRetryReason::Connection]
                    .into_iter()
                    .collect(),
                vec![Duration::ZERO; 3],
                Duration::ZERO,
            )),
            None,
        )
    }
    #[tokio::test]
    async fn history_rebuild_transient_failure_keeps_full_context_on_next_attempt() {
        let inner = history_script(
            vec![
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Err(ModelError::Unavailable {
                    message: "center proxy temporarily unavailable".into(),
                    status: Some(503),
                    retry_after_ms: None,
                }),
                Ok(history_finished()),
            ],
            false,
        );
        let model = CallScopedHistoryModelService::new(
            inner.clone(),
            Some(agent_model::ModelRetryPolicy::new(
                [
                    agent_model::ModelRetryReason::Connection,
                    agent_model::ModelRetryReason::Unavailable,
                ]
                .into_iter()
                .collect(),
                vec![Duration::ZERO; 3],
                Duration::ZERO,
            )),
            None,
        );
        agent_model::collect_model_turn(&model, history_request(), ModelCallContext::default())
            .await
            .unwrap();
        let calls = inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(_, c)| c.rebuild_server_history)
                .collect::<Vec<_>>(),
            [false, false, true, true]
        );
        assert!(
            calls
                .iter()
                .all(|(request, _)| request == &history_request())
        );
    }
    #[tokio::test]
    async fn history_rebuild_temporary_failures_exhaust_existing_retry_budget() {
        let unavailable = ModelError::Unavailable {
            message: "temporary proxy failure".into(),
            status: Some(503),
            retry_after_ms: None,
        };
        let inner = history_script(
            vec![
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Err(unavailable.clone()),
                Err(unavailable.clone()),
                Err(unavailable.clone()),
            ],
            false,
        );
        let model = CallScopedHistoryModelService::new(
            inner.clone(),
            Some(agent_model::ModelRetryPolicy::new(
                [
                    agent_model::ModelRetryReason::Connection,
                    agent_model::ModelRetryReason::Unavailable,
                ]
                .into_iter()
                .collect(),
                vec![Duration::ZERO; 3],
                Duration::ZERO,
            )),
            None,
        );
        assert_eq!(
            agent_model::collect_model_turn(&model, history_request(), ModelCallContext::default())
                .await
                .unwrap_err(),
            unavailable
        );
        let calls = inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(_, c)| c.rebuild_server_history)
                .collect::<Vec<_>>(),
            [false, false, true, true, true]
        );
    }
    #[tokio::test]
    async fn history_rebuild_retry_cancellation_prevents_another_request() {
        struct CancelRetry(tokio_util::sync::CancellationToken);
        impl ModelAttemptObserver for CancelRetry {
            fn observe(&self, event: ModelAttemptEvent) {
                if matches!(
                    event,
                    ModelAttemptEvent::RetryScheduled {
                        next_attempt: 3,
                        ..
                    }
                ) {
                    self.0.cancel();
                }
            }
        }
        let cancellation = tokio_util::sync::CancellationToken::new();
        let inner = history_script(
            vec![
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Err(ModelError::Unavailable {
                    message: "temporary proxy failure".into(),
                    status: Some(503),
                    retry_after_ms: None,
                }),
            ],
            false,
        );
        let model = CallScopedHistoryModelService::new(
            inner.clone(),
            Some(agent_model::ModelRetryPolicy::new(
                [
                    agent_model::ModelRetryReason::Connection,
                    agent_model::ModelRetryReason::Unavailable,
                ]
                .into_iter()
                .collect(),
                vec![Duration::ZERO; 3],
                Duration::ZERO,
            )),
            Some(Arc::new(CancelRetry(cancellation.clone()))),
        );
        assert_eq!(
            agent_model::collect_model_turn(
                &model,
                history_request(),
                ModelCallContext::new(cancellation)
            )
            .await
            .unwrap_err(),
            ModelError::Cancelled
        );
        assert_eq!(inner.calls.lock().unwrap().len(), 3);
    }
    #[tokio::test]
    async fn history_recovery_after_stream_retry_renews_for_each_logical_call() {
        let inner = history_script(
            vec![
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Ok(history_finished()),
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Ok(history_finished()),
            ],
            false,
        );
        let model = call_scoped_history(inner.clone());
        for _ in 0..2 {
            agent_model::collect_model_turn(&model, history_request(), ModelCallContext::default())
                .await
                .unwrap();
        }
        let calls = inner.calls.lock().unwrap();
        assert_eq!(calls.len(), 6);
        assert_eq!(
            calls
                .iter()
                .map(|(_, c)| c.rebuild_server_history)
                .collect::<Vec<_>>(),
            [false, false, true, false, false, true]
        );
        assert!(
            calls
                .iter()
                .all(|(request, _)| request == &history_request())
        );
    }
    #[tokio::test]
    async fn history_recovery_has_one_allowance_across_stream_retry_attempts() {
        let inner = history_script(
            vec![
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
                Ok(history_interrupted()),
                Err(ModelError::HistoryUnavailable),
            ],
            false,
        );
        let model = call_scoped_history(inner.clone());
        assert_eq!(
            agent_model::collect_model_turn(&model, history_request(), ModelCallContext::default())
                .await
                .unwrap_err(),
            ModelError::HistoryUnavailable
        );
        assert_eq!(inner.calls.lock().unwrap().len(), 4);
    }
    #[tokio::test]
    async fn history_recovery_failed_rebuild_is_not_recursively_rebuilt() {
        let inner = history_script(
            vec![
                Err(ModelError::HistoryUnavailable),
                Err(ModelError::HistoryUnavailable),
            ],
            false,
        );
        let model = call_scoped_history(inner.clone());
        assert!(matches!(
            model
                .stream(history_request(), ModelCallContext::default())
                .await,
            Err(ModelError::HistoryUnavailable)
        ));
        assert_eq!(inner.calls.lock().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn history_recovery_preserves_full_request_and_context_and_one_call_allowance() {
        let inner = history_script(
            vec![
                Err(ModelError::HistoryUnavailable),
                Ok(vec![]),
                Err(ModelError::HistoryUnavailable),
            ],
            false,
        );
        let allowance = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let service = RecoveringHistoryModelService::new(inner.clone(), allowance.clone());
        let context =
            ModelCallContext::default().with_trace(agent_model::TraceContext::new("trace-history"));
        let request = history_request();
        assert!(
            service
                .stream(request.clone(), context.clone())
                .await
                .is_ok()
        );
        // 同一逻辑调用的重试共享额度，不允许重复重建。
        let rebound = RecoveringHistoryModelService::new(inner.clone(), allowance);
        assert!(matches!(
            rebound.stream(request.clone(), context.clone()).await,
            Err(ModelError::HistoryUnavailable)
        ));
        let calls = inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(_, c)| c.rebuild_server_history)
                .collect::<Vec<_>>(),
            [false, true, true]
        );
        for (actual, actual_context) in calls.iter() {
            assert_eq!(actual, &request);
            assert_eq!(actual_context.trace, context.trace);
            assert_eq!(
                actual_context.prepared_images.len(),
                context.prepared_images.len()
            );
            assert!(!actual_context.cancellation.is_cancelled());
        }
    }
    #[tokio::test]
    async fn history_recovery_does_not_retry_ordinary_errors_stream_failures_or_cancellation() {
        for error in [
            ModelError::Auth("denied".into()),
            ModelError::Provider {
                message: "ordinary invalid request".into(),
                status: Some(400),
            },
            ModelError::Transport {
                kind: agent_model::ModelTransportErrorKind::Timeout,
                message: "timeout".into(),
            },
        ] {
            let inner = history_script(vec![Err(error.clone())], false);
            let service = RecoveringHistoryModelService::new(inner.clone(), Default::default());
            assert!(
                matches!(service.stream(history_request(), ModelCallContext::default()).await, Err(actual) if actual == error)
            );
            assert_eq!(inner.calls.lock().unwrap().len(), 1);
        }
        let error = agent_model::ModelEvent::TurnFailed {
            error: ModelError::HistoryUnavailable,
        };
        let inner = history_script(vec![Ok(vec![error.clone()])], false);
        let service = RecoveringHistoryModelService::new(inner.clone(), Default::default());
        let events = futures_util::StreamExt::collect::<Vec<_>>(
            service
                .stream(history_request(), ModelCallContext::default())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(events, vec![error]);
        assert_eq!(inner.calls.lock().unwrap().len(), 1);
        let inner = history_script(vec![Err(ModelError::HistoryUnavailable)], true);
        let service = RecoveringHistoryModelService::new(inner.clone(), Default::default());
        assert!(matches!(
            service
                .stream(history_request(), ModelCallContext::default())
                .await,
            Err(ModelError::Cancelled)
        ));
        assert_eq!(inner.calls.lock().unwrap().len(), 1);
    }
}
