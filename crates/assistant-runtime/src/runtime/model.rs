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
use assistant_protocol::{AgentVariant, ApprovalMode, ModelKey};

use super::AssistantRuntime;
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
    pub(super) model_id: String,
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
    model_key: String,
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
                                model_key: self.model_key.clone(),
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
    agent: Agent,
    authorizer: Arc<dyn ToolAuthorizer>,
    compactor: Arc<RuntimeContextCompactor>,
    reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    goal_signal_latch: Option<Arc<GoalRunSignalLatch>>,
    skill_activation_latch: Arc<SkillActivationLatch>,
}

pub(super) struct CompiledRunParts {
    pub(super) agent: Agent,
    pub(super) authorizer: Arc<dyn ToolAuthorizer>,
    pub(super) compactor: Arc<RuntimeContextCompactor>,
    pub(super) reasoning_effort: Option<assistant_protocol::ReasoningEffortKey>,
    pub(super) goal_signal_latch: Option<Arc<GoalRunSignalLatch>>,
    pub(super) skill_activation_latch: Arc<SkillActivationLatch>,
}

impl CompiledRunAgent {
    pub(super) fn into_parts(self) -> CompiledRunParts {
        CompiledRunParts {
            agent: self.agent,
            authorizer: self.authorizer,
            compactor: self.compactor,
            reasoning_effort: self.reasoning_effort,
            goal_signal_latch: self.goal_signal_latch,
            skill_activation_latch: self.skill_activation_latch,
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
}

/// 队列驱动与历史重入共同传入的 Run 装配资源；收敛参数数量并明确哪些能力来自 Runtime。
pub(super) struct RunCompilationResources<'a> {
    pub(super) model_factory: &'a dyn crate::ModelServiceFactory,
    pub(super) context_window: Arc<agent_sdk::ContextWindowEvaluator>,
    pub(super) run_tool_factory: &'a dyn RunToolFactory,
    pub(super) child_task_workspace_factory: Arc<dyn ChildTaskWorkspaceFactory>,
    pub(super) child_tasks: Arc<ChildTaskRegistry>,
    pub(super) store: Arc<dyn RuntimeStore>,
    pub(super) recall_reference_codec: Arc<crate::HmacRecallReferenceCodec>,
}

impl AssistantRuntime {
    /// 从同一配置快照构造 Run 和连接验证共用的冻结 ModelService。
    pub(super) fn compile_model_service(
        &self,
        snapshot: &ConfigSnapshot,
        model_key: &assistant_protocol::ModelKey,
    ) -> RuntimeResult<CompiledModelService> {
        compile_model_service(snapshot, model_key, self.model_factory.as_ref())
    }
}

pub(super) fn compile_run_agent(
    session: Arc<SessionController>,
    snapshot: &ConfigSnapshot,
    resources: RunCompilationResources<'_>,
    authorization: RunAuthorizationInput,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledRunAgent> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let model_key = session.model_key()?;
    let model_config = resolve_model(snapshot, &model_key)?;
    let mut compiled = compile_model_service_with_observer(
        snapshot,
        &model_key,
        resources.model_factory,
        model_attempt_observer,
    )?;
    let image_inspector: Option<agent_tools::SharedImageInspector> =
        if !compiled.capabilities.image_input && compiled.capabilities.tool_calls {
            active.vision().and_then(|vision| {
                let mut auxiliary =
                    compile_model_service(snapshot, &vision.model_key, resources.model_factory)
                        .ok()?;
                if !auxiliary.capabilities.image_input {
                    return None;
                }
                let image_preprocessor = auxiliary.image_preprocessor.clone()?;
                bind_image_preparation(&mut auxiliary, session.environment());
                let (reasoning, provider_options) = protocol_request_options(
                    &auxiliary.provider,
                    auxiliary.protocol,
                    &auxiliary.model_id,
                    &auxiliary.capabilities,
                    None,
                )
                .ok()?;
                Some(Arc::new(AuxiliaryVisionInspector {
                    model_key: vision.model_key.as_str().to_owned(),
                    model: auxiliary.model,
                    image_preprocessor,
                    reasoning,
                    provider_options,
                    timeout: vision.timeout,
                    max_output_tokens: vision.max_output_tokens.min(auxiliary.max_output_tokens),
                }) as agent_tools::SharedImageInspector)
            })
        } else {
            None
        };
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
        &compiled.model_id,
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
        state
            .skill_activations
            .iter()
            .filter(|activation| {
                matches!(
                    &activation.owner,
                    SkillActivationOwner::Session(owner) if owner == session.id()
                )
            })
            .map(|activation| activation.name.clone())
            .collect::<Vec<_>>()
    };
    let skill_activation_latch = Arc::new(SkillActivationLatch::new(active_skill_names));
    let parent_compactor = Arc::new(RuntimeContextCompactor::for_parent(
        compiled.model.clone(),
        session.system_prompt().clone(),
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
    let authorizer = Arc::new(
        RuntimeToolAuthorizer::new(
            RunAuthorizationScope {
                variant: authorization.variant,
                approval_mode: authorization.approval_mode,
            },
            session.permission_scopes(),
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
        .with_goal_signal_latch(goal_signal_latch.clone()),
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
    let parent_tools = if compiled.model.capabilities().tool_calls {
        let parent_tools = base_tools
            .clone()
            .try_with_tool(LoadSkillTool::new(
                session.skill_catalog().clone(),
                skill_activation_latch.clone(),
            ))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "load skill tool definition",
            })?
            .try_with_tool(UpdatePlanTool::new(
                session.clone(),
                resources.store.clone(),
                authorization.events.clone(),
            ))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "update plan tool definition",
            })?
            .try_with_tool(UpdateGoalTool::new(goal_signal_latch.clone()))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "update goal tool definition",
            })?;
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
        let mut child_prompt_parts = session.system_prompt().parts().to_vec();
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
        .tools(base_tools.clone())
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
                skill_catalog: session.skill_catalog().clone(),
            }));
        parent_tools
            .try_with_tool(DelegateTaskTool::new(delegation_controller))
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "delegate task tool definition",
            })?
    } else {
        base_tools
    };

    let mut builder = AgentBuilder::new(
        compiled.model,
        session.system_prompt().clone(),
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
    Ok(CompiledRunAgent {
        agent,
        authorizer,
        compactor: parent_compactor,
        reasoning_effort: frozen_reasoning_effort,
        goal_signal_latch,
        skill_activation_latch,
    })
}

pub(super) fn compile_model_service(
    snapshot: &ConfigSnapshot,
    model_key: &assistant_protocol::ModelKey,
    model_factory: &dyn crate::ModelServiceFactory,
) -> RuntimeResult<CompiledModelService> {
    compile_model_service_with_observer(snapshot, model_key, model_factory, None)
}

fn compile_model_service_with_observer(
    snapshot: &ConfigSnapshot,
    model_key: &assistant_protocol::ModelKey,
    model_factory: &dyn crate::ModelServiceFactory,
    model_attempt_observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> RuntimeResult<CompiledModelService> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let model_config = resolve_model(snapshot, model_key)?;
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
        model_id: model_config.model().to_owned(),
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
    model_id: &str,
    capabilities: &ResolvedModelCapabilities,
    requested_effort: Option<assistant_protocol::ReasoningEffortKey>,
) -> RuntimeResult<(Option<ReasoningConfig>, ProviderOptions)> {
    let mut provider_options = ProviderOptions::new();
    if protocol == ModelProtocol::OpenAiChatCompletions && capabilities.reasoning_enabled() {
        let options = match provider.as_str() {
            "deepseek" | "zhipu" => Some(serde_json::json!({"thinking": {"type": "enabled"}})),
            // K3 始终思考且官方要求从 K2.x 迁移时移除 `thinking`；K2.x 才发送开关。
            "moonshot" if !matches!(model_id, "kimi-k3" | "k3") => {
                Some(serde_json::json!({"thinking": {"type": "enabled"}}))
            }
            "dashscope" if model_id == "qwen3.8-max" => {
                Some(serde_json::json!({"enable_thinking": true, "preserve_thinking": true}))
            }
            "dashscope" => Some(serde_json::json!({"enable_thinking": true})),
            _ => None,
        };
        if let Some(options) = options {
            provider_options
                .insert(provider.as_str(), options)
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "static reasoning provider options",
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

/// CreateSession 在同一配置快照中解析显式或默认 model key。
pub(super) fn resolve_session_model_key(
    snapshot: &ConfigSnapshot,
    requested: Option<ModelKey>,
) -> RuntimeResult<ModelKey> {
    let active = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    let key = requested
        .or_else(|| active.default_model().cloned())
        .ok_or(RuntimeError::ConfigurationUnavailable)?;
    resolve_model(snapshot, &key)?;
    Ok(key)
}

/// 在安全投影和有效 map 之间区分不存在与存在但无效。
fn resolve_model<'a>(
    snapshot: &'a ConfigSnapshot,
    key: &ModelKey,
) -> RuntimeResult<&'a ResolvedModelConfig> {
    if let Some(model) = snapshot.model(key) {
        return Ok(model);
    }
    if snapshot.contains_model_key(key) {
        Err(RuntimeError::ModelUnavailable {
            model_key: key.clone(),
        })
    } else {
        Err(RuntimeError::ModelNotFound {
            model_key: key.clone(),
        })
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
