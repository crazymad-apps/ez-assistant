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
use assistant_protocol::{AgentVariant, ApprovalMode};

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

/// 一次配置快照编译出的模型调用边界。
///
/// Run 和连接验证共用这条构造链，避免两者对 endpoint、credential、协议 Adapter、
/// timeout 和 retry 产生不同解释；两者的请求内容仍分别构造。
pub(super) struct CompiledModelService {
    pub(super) model: Arc<dyn ModelService>,
    pub(super) provider: agent_types::ProviderId,
    pub(super) protocol: ModelProtocol,
    pub(super) capabilities: ResolvedModelCapabilities,
    pub(super) max_output_tokens: u32,
    pub(super) request_timeout: Duration,
    pub(super) image_preprocessor: Option<Arc<dyn ModelImagePreprocessor>>,
}

/// 未启用重试时只补充 attempt 观察，不改变下层取消、超时或建流语义。
struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    observer: Arc<dyn ModelAttemptObserver>,
}

/// 在协议服务与有限重试之外一次性准备本次调用需要的全部图片。
struct ImagePreparingModelService {
    inner: Arc<dyn ModelService>,
    preprocessor: Arc<dyn ModelImagePreprocessor>,
    tool_image_directory: String,
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

impl ModelService for ImagePreparingModelService {
    fn capabilities(&self) -> &agent_model::ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn stream(
        &self,
        request: agent_model::ModelRequest,
        mut context: agent_model::ModelCallContext,
    ) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
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
                    match self
                        .preprocessor
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
}

pub(super) struct CompiledRunParts {
    pub(super) model_binding: Arc<crate::config::PreparedModel>,
    pub(super) agent: Agent,
    pub(super) authorizer: Arc<dyn ToolAuthorizer>,
    pub(super) compactor: Arc<RuntimeContextCompactor>,
    pub(super) reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    pub(super) goal_signal_latch: Option<Arc<GoalRunSignalLatch>>,
    pub(super) skill_activation_latch: Arc<SkillActivationLatch>,
    pub(super) can_speak: bool,
    pub(super) disclosure_context: Option<agent_types::UserMessage>,
}

impl CompiledRunAgent {
    /// 调用方取得配置接纳门禁后核验，直到 Run 领取完成前不得释放该门禁。
    pub(super) fn ensure_models_current(
        &self,
        registry: &crate::config::ConfigRegistry,
    ) -> RuntimeResult<()> {
        self.prepared_model.ensure_current(registry)?;
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
            model_binding: Arc::new(self.prepared_model),
            agent: self.agent,
            authorizer: self.authorizer,
            compactor: self.compactor,
            reasoning_effort: self.reasoning_effort,
            goal_signal_latch: self.goal_signal_latch,
            skill_activation_latch: self.skill_activation_latch,
            can_speak: self.can_speak,
            disclosure_context: self.disclosure_context,
        }
    }
}

pub(super) struct RunAuthorizationInput {
    pub(super) permission_coordinator: Arc<PermissionCoordinator>,
    pub(super) approval_registry: Arc<ApprovalRegistry>,
    pub(super) variant: AgentVariant,
    pub(super) approval_mode: ApprovalMode,
    pub(super) run_id: assistant_protocol::RunId,
    pub(super) cancellation: tokio_util::sync::CancellationToken,
    pub(super) events: ObservationCoordinator,
    pub(super) goal_binding: Option<GoalRunBinding>,
    pub(super) input_origin: crate::InputOrigin,
    pub(super) cross_session: Option<crate::CrossSessionInputEnvelope>,
}

/// 队列驱动与历史重入共同传入的 Run 装配资源；收敛参数数量并明确哪些能力来自 Runtime。
pub(super) struct RunCompilationResources<'a> {
    pub(super) skill_catalog: crate::SkillCatalog,
    pub(super) model_factory: &'a dyn crate::ModelServiceFactory,
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

/// 复用 Session 已接纳的模型参数；配置改变或进程重启后只读固定配置，不隐式联网。
/// 调用方负责在既有 Session／配置门禁内接纳结果；本函数不修改 Session 或 Store。
pub(super) async fn resolve_session_model(
    registry: &crate::config::ConfigRegistry,
    session: &SessionController,
    selection: Option<&assistant_protocol::ModelSelection>,
    store: &dyn RuntimeStore,
) -> RuntimeResult<Arc<crate::config::PreparedModel>> {
    let binding = session.lock_state()?.model_binding.clone();
    let effective =
        selection
            .cloned()
            .or(registry.managed_models()?.settings.default_model.clone());
    if let Some(binding) = binding
        && Some(&binding.selection) == effective.as_ref()
        && binding.ensure_current(registry).is_ok()
    {
        return Ok(binding);
    }
    let snapshot = registry.snapshot()?;
    registry
        .configured_model(&snapshot, selection, store)
        .await
        .map(Arc::new)
}

impl AssistantRuntime {
    /// 为独立手动压缩冻结当前 Session 的模型服务；不装配 Agent 或工具。
    pub(super) async fn compile_session_compactor(
        &self,
        session: &SessionController,
        selection: Option<&assistant_protocol::ModelSelection>,
    ) -> RuntimeResult<(RuntimeContextCompactor, Arc<crate::config::PreparedModel>)> {
        let snapshot = self.config_registry.snapshot()?;
        let prepared = resolve_session_model(
            &self.config_registry,
            session,
            selection,
            self.store.as_ref(),
        )
        .await?;
        let mut compiled = compile_resolved_model_service(
            &snapshot,
            &prepared.model,
            self.model_factory.as_ref(),
            None,
        )?;
        bind_image_preparation(&mut compiled, session.environment());
        Ok((
            RuntimeContextCompactor::for_manual(compiled.model, session.current_system_prompt()?),
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
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let selection = session.model_selection()?;
    let prepared = registry
        .prepare_model(
            snapshot,
            selection.as_ref(),
            resources.store.as_ref(),
            resources.model_factory,
        )
        .await?;
    let model_config = &prepared.model;
    let mut compiled = compile_resolved_model_service(
        snapshot,
        model_config,
        resources.model_factory,
        model_attempt_observer,
    )?;
    let auxiliary_selection = registry.managed_models()?.settings.vision_model.clone();
    let mut auxiliary_prepared = None;
    let image_inspector: Option<agent_tools::SharedImageInspector> =
        if !compiled.capabilities.image_input && compiled.capabilities.tool_calls {
            if let Some(selection) = &auxiliary_selection {
                let prepared_auxiliary = registry
                    .prepare_model(
                        snapshot,
                        Some(selection),
                        resources.store.as_ref(),
                        resources.model_factory,
                    )
                    .await?;
                let mut auxiliary = compile_resolved_model_service(
                    snapshot,
                    &prepared_auxiliary.model,
                    resources.model_factory,
                    None,
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
                bind_image_preparation(&mut auxiliary, session.environment());
                let (reasoning, provider_options) = protocol_request_options(
                    &auxiliary.provider,
                    auxiliary.protocol,
                    &auxiliary.capabilities,
                    None,
                )?;
                let timeout = active
                    .vision()
                    .map_or(Duration::from_secs(60), |vision| vision.timeout);
                let output_budget = active
                    .vision()
                    .map_or(4096, |vision| vision.max_output_tokens);
                auxiliary_prepared = Some(prepared_auxiliary);
                Some(Arc::new(AuxiliaryVisionInspector {
                    selection: selection.clone(),
                    model: auxiliary.model,
                    image_preprocessor,
                    reasoning,
                    provider_options,
                    timeout,
                    max_output_tokens: output_budget.min(auxiliary.max_output_tokens),
                }) as agent_tools::SharedImageInspector)
            } else {
                None
            }
        } else {
            None
        };
    prepared.ensure_current(registry)?;
    if let Some(auxiliary) = &auxiliary_prepared {
        auxiliary.ensure_current(registry)?;
    }
    bind_image_preparation(&mut compiled, session.environment());
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
    let bundle = resources
        .run_tool_factory
        .compile(crate::RunToolFactoryRequest {
            session_id: session.id(),
            environment: session.environment(),
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
    let parent_compactor = Arc::new(RuntimeContextCompactor::for_parent(
        compiled.model.clone(),
        session.current_system_prompt()?,
    ));
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
    let mcp_scope = mcp_disclosure.scope.clone();
    let authorizer = Arc::new(
        RuntimeToolAuthorizer::new(
            RunAuthorizationScope {
                variant: authorization.variant,
                approval_mode: authorization.approval_mode,
            },
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
                approval_mode: authorization.approval_mode,
                workspace_id: session.environment().workspace_id.clone(),
                cancellation: authorization.cancellation.clone(),
                events: authorization.events.clone(),
            }),
        )?
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
    let mut child_base_tools = base_tools.clone();
    if compiled.model.capabilities().tool_calls {
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
        let child_compactor = Arc::new(RuntimeContextCompactor::for_child(
            compiled.model.clone(),
            child_prompt.clone(),
        ));
        let mut child_builder = AgentBuilder::new(
            compiled.model.clone(),
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
                session: session.clone(),
                parent_run_id: authorization.run_id,
                variant: authorization.variant,
                approval_mode: authorization.approval_mode,
                child_agent,
                child_compactor,
                store: resources.store,
                registry: resources.child_tasks,
                workspace_factory: resources.child_task_workspace_factory,
                permission_coordinator: authorization.permission_coordinator,
                approval_registry: authorization.approval_registry,
                infrastructure_policies,
                events: authorization.events,
                limits: delegation,
                skill_catalog: resources.skill_catalog.clone(),
                mcp_registry: resources.mcp_registry.clone(),
                disclosure_context: mcp_disclosure.context.clone(),
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
    };
    result.ensure_models_current(registry)?;
    Ok(result)
}

pub(super) fn compile_resolved_model_service(
    snapshot: &ConfigSnapshot,
    model_config: &ResolvedModelConfig,
    model_factory: &dyn crate::ModelServiceFactory,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledModelService> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let transport = active.transport();
    let bundle = model_factory
        .create_model(ModelServiceFactoryRequest {
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
        .map_err(|source| RuntimeError::ModelBuildFailed { source })?;
    let base_model = bundle.model;
    let image_preprocessor = bundle.image_preprocessor;
    let model = match (active.retry_policy(), model_attempt_observer) {
        (Some(policy), Some(observer)) => Arc::new(RetryingModelService::with_observer(
            base_model,
            policy.clone(),
            observer,
        )) as Arc<dyn ModelService>,
        (Some(policy), None) => {
            Arc::new(RetryingModelService::new(base_model, policy.clone())) as Arc<dyn ModelService>
        }
        (None, Some(observer)) => Arc::new(ObservedModelService {
            inner: base_model,
            observer,
        }) as Arc<dyn ModelService>,
        (None, None) => base_model,
    };
    Ok(CompiledModelService {
        model,
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
) {
    let Some(preprocessor) = compiled.image_preprocessor.take() else {
        return;
    };
    if !compiled.capabilities.image_input {
        return;
    }
    compiled.model = Arc::new(ImagePreparingModelService {
        inner: compiled.model.clone(),
        preprocessor,
        tool_image_directory: environment.session_tool_image_directory.clone(),
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
        let service = ImagePreparingModelService {
            inner: Arc::new(ProviderReachedModel::default()),
            preprocessor: Arc::new(AlwaysImagePreprocessor),
            tool_image_directory: String::new(),
        };

        let result = service.stream(request, ModelCallContext::default()).await;

        assert!(matches!(
            result,
            Err(ModelError::Provider { message, status: Some(400) })
                if message == "provider image limit"
        ));
    }
}
