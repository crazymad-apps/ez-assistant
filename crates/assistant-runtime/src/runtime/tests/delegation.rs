use super::*;

struct DelegationScenarioModel {
    capabilities: ModelCapabilities,
    tasks: Vec<String>,
    blocked_tasks: Vec<String>,
    approval_tasks: Vec<String>,
    child_delay: Duration,
    active_children: Arc<AtomicUsize>,
    peak_children: Arc<AtomicUsize>,
    child_sequence: AtomicUsize,
    parent_requests: AtomicUsize,
}

impl DelegationScenarioModel {
    fn delayed(tasks: &[&str], child_delay: Duration) -> Self {
        Self {
            capabilities: model_capabilities(true),
            tasks: tasks.iter().map(|task| (*task).to_owned()).collect(),
            blocked_tasks: Vec::new(),
            approval_tasks: Vec::new(),
            child_delay,
            active_children: Arc::new(AtomicUsize::new(0)),
            peak_children: Arc::new(AtomicUsize::new(0)),
            child_sequence: AtomicUsize::new(0),
            parent_requests: AtomicUsize::new(0),
        }
    }

    fn with_blocked_tasks(mut self, tasks: &[&str]) -> Self {
        self.blocked_tasks = tasks.iter().map(|task| (*task).to_owned()).collect();
        self
    }

    fn with_approval_tasks(mut self, tasks: &[&str]) -> Self {
        self.approval_tasks = tasks.iter().map(|task| (*task).to_owned()).collect();
        self
    }

    fn peak_children(&self) -> usize {
        self.peak_children.load(Ordering::SeqCst)
    }

    fn parent_requests(&self) -> usize {
        self.parent_requests.load(Ordering::SeqCst)
    }
}

struct ActiveChildCounter {
    active: Arc<AtomicUsize>,
}

impl Drop for ActiveChildCounter {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ModelService for DelegationScenarioModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let is_parent = request
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "delegate_task");
        if is_parent {
            self.parent_requests.fetch_add(1, Ordering::SeqCst);
            let has_results = request
                .conversation
                .messages
                .iter()
                .any(|message| matches!(message, ConversationMessage::Tool(_)));
            let message = if has_results {
                assistant_text("parent-scenario-final", "parent completed")
            } else {
                delegate_batch_message(&self.tasks)
            };
            return Box::pin(async move {
                Ok(
                    Box::pin(futures_util::stream::iter(message_events(&message)))
                        as agent_model::ModelEventStream,
                )
            });
        }

        let task = request
            .conversation
            .messages
            .iter()
            .find_map(|message| match message {
                ConversationMessage::User(message) => message.parts.iter().find_map(|part| {
                    matches!(part, UserPart::Text(_)).then(|| match part {
                        UserPart::Text(text) => text.text.clone(),
                        UserPart::FileReferences(_) | UserPart::Injected(_) => unreachable!(),
                    })
                }),
                ConversationMessage::System(_)
                | ConversationMessage::ContextSummary(_)
                | ConversationMessage::Assistant(_)
                | ConversationMessage::Tool(_) => None,
            })
            .unwrap_or_else(|| "unknown child task".to_owned());
        let blocked = self.blocked_tasks.iter().any(|value| value == &task);
        let needs_approval = self.approval_tasks.iter().any(|value| value == &task)
            && !request
                .conversation
                .messages
                .iter()
                .any(|message| matches!(message, ConversationMessage::Tool(_)));
        let delay = self.child_delay;
        let active = self.active_children.clone();
        let peak = self.peak_children.clone();
        let sequence = self.child_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            let entered = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(entered, Ordering::SeqCst);
            let _active = ActiveChildCounter { active };
            if blocked {
                context.cancellation.cancelled().await;
                return Err(ModelError::Cancelled);
            }
            if needs_approval {
                let message = AssistantMessage {
                    id: MessageId::new(format!("child-approval-{sequence}")).expect("message id"),
                    model: ModelIdentity::new(
                        ProviderId::new("fixture").expect("provider id"),
                        "fixture-model",
                    ),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new(format!("child-approval-call-{sequence}"))
                            .expect("tool call id"),
                        name: ToolName::new("child_probe").expect("tool name"),
                        arguments: json!({"task": task}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                };
                return Ok(
                    Box::pin(futures_util::stream::iter(message_events(&message)))
                        as agent_model::ModelEventStream,
                );
            }
            tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(ModelError::Cancelled),
                () = tokio::time::sleep(delay) => {
                    let message = assistant_text(
                        &format!("child-scenario-{sequence}"),
                        &format!("result for {task}"),
                    );
                    Ok(Box::pin(futures_util::stream::iter(message_events(&message)))
                        as agent_model::ModelEventStream)
                }
            }
        })
    }
}

fn delegate_batch_message(tasks: &[String]) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("parent-delegate-batch").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: tasks
            .iter()
            .enumerate()
            .map(|(index, task)| {
                AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId::new(format!("delegate-batch-{index}")).expect("tool call id"),
                    name: ToolName::new("delegate_task").expect("tool name"),
                    arguments: json!({
                        "title": task,
                        "task": task,
                    }),
                })
            })
            .collect(),
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn config_with_delegation(max_tasks: u32, max_concurrent: u32, timeout_ms: u64) -> String {
    config_with_delegation_limits(max_tasks, max_concurrent, timeout_ms, 40, 100, 4_096)
}

fn config_with_delegation_limits(
    max_tasks: u32,
    max_concurrent: u32,
    timeout_ms: u64,
    max_steps: u32,
    max_tool_calls: u32,
    max_output_tokens: u32,
) -> String {
    format!(
        "{TEST_CONFIG}\n[agent.defaults.delegation]\nmax_tasks_per_run = {max_tasks}\nmax_concurrent_tasks = {max_concurrent}\ntask_timeout_ms = {timeout_ms}\nmax_steps = {max_steps}\nmax_tool_calls = {max_tool_calls}\nmax_output_tokens = {max_output_tokens}\n"
    )
}

fn delegation_runtime_config() -> RuntimeConfig {
    RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero event capacity"))
}

async fn wait_for_child_count(
    store: &dyn RuntimeStore,
    count: usize,
) -> Vec<crate::StoredChildTask> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let tasks = store
                .load_runtime()
                .await
                .expect("child projection")
                .child_tasks;
            if tasks.len() >= count {
                return tasks;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child tasks are created")
}

#[tokio::test]
async fn one_delegate_task_runs_an_isolated_child_and_returns_only_its_final_result() {
    let delegate_call = AssistantMessage {
        id: MessageId::new("parent-delegate").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("delegate-call-1").expect("call id"),
            name: ToolName::new("delegate_task").expect("tool name"),
            arguments: json!({
                "title": "Inspect one fact",
                "task": "Return the delegated answer.",
                "context": "Only use the supplied context.",
                "expected_output": "One concise sentence."
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let child_usage = agent_types::TokenUsage {
        input_tokens: 640,
        output_tokens: 64,
        total_tokens: 704,
        cached_input_tokens: Some(512),
        reasoning_tokens: Some(16),
    };
    let mut child_final = assistant_text("child-final", "delegated answer");
    child_final.usage = Some(child_usage.clone());
    let parent_final = assistant_text("parent-final", "parent received delegated answer");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate_call)),
            ModelScript::Events(message_events(&child_final)),
            ModelScript::Events(message_events(&parent_final)),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let workspaces = Arc::new(TestChildWorkspaceFactory::default());
    let runtime =
        runtime_with_store_and_child_workspaces(model.clone(), store.clone(), workspaces.clone())
            .await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation_limits(
            8, 4, 300_000, 40, 100, 1_024,
        ));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let mut events = runtime.subscribe_events();
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate this".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(terminal.text, "parent received delegated answer");

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "delegate_task")
    );
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "update_plan")
    );
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "update_goal")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .all(|definition| definition.name.as_str() != "delegate_task")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .all(|definition| definition.name.as_str() != "update_plan")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .all(|definition| definition.name.as_str() != "update_goal")
    );
    assert_eq!(requests[1].conversation.messages.len(), 1);
    let ConversationMessage::User(child_input) = &requests[1].conversation.messages[0] else {
        panic!("child must start from its independent user input");
    };
    assert!(child_input.parts.iter().any(|part| {
        matches!(part, UserPart::Text(text) if text.text == "Return the delegated answer.")
    }));
    assert!(!child_input.parts.iter().any(|part| {
        matches!(part, UserPart::Text(text) if text.text.contains("delegate this"))
    }));
    assert!(
        requests[1]
            .system
            .parts()
            .iter()
            .any(|part| part.contains("non-recursive child agent"))
    );
    assert_eq!(requests[0].generation.max_output_tokens, Some(4_096));
    assert_eq!(requests[1].generation.max_output_tokens, Some(1_024));

    let recovered = store.load_runtime().await.expect("child projection");
    assert_eq!(recovered.child_tasks.len(), 1);
    let child = &recovered.child_tasks[0];
    assert_eq!(
        child.status,
        assistant_protocol::ChildTaskStatus::Completed,
        "child error: {:?}",
        child.error
    );
    assert_eq!(child.parent_run_id, started.run.run_id);
    assert_eq!(child.parent_tool_call_id.as_str(), "delegate-call-1");
    assert_eq!(child.final_message_id.as_ref(), Some(&child_final.id));
    let child_conversation = store
        .load_child_conversation(&session.session.session_id, &child.child_task_id)
        .await
        .expect("child conversation");
    agent_context::ContextLayout::build(&child_conversation)
        .expect("compacted child conversation remains structurally valid");
    assert_eq!(child_conversation.messages.len(), 2);
    assert_eq!(
        child_conversation.messages[1],
        ConversationMessage::Assistant(child_final)
    );
    let ConversationMessage::Assistant(persisted_child_final) = &child_conversation.messages[1]
    else {
        unreachable!("child final message is assistant")
    };
    assert_eq!(persisted_child_final.usage.as_ref(), Some(&child_usage));

    let parent_conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("parent conversation");
    assert_eq!(parent_conversation.messages.len(), 4);
    parent_conversation
        .validate_tool_exchange_pairs()
        .expect("parent delegate call/result pair");
    assert!(
        !parent_conversation
            .messages
            .iter()
            .any(|message| message == &child_conversation.messages[1])
    );

    let released = workspaces
        .released_paths
        .lock()
        .expect("released paths")
        .clone();
    assert_eq!(released.len(), 1);
    assert!(!std::path::Path::new(&released[0]).exists());
    let observed = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert!(observed.iter().any(|event| matches!(
        event,
        assistant_protocol::RuntimeEvent::ChildTaskEvent {
            event: assistant_protocol::ChildTaskEvent::Created { .. },
            ..
        }
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        assistant_protocol::RuntimeEvent::ChildTaskEvent {
            event: assistant_protocol::ChildTaskEvent::Started,
            ..
        }
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        assistant_protocol::RuntimeEvent::ChildTaskEvent {
            event: assistant_protocol::ChildTaskEvent::TextDelta { .. },
            ..
        }
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        assistant_protocol::RuntimeEvent::ChildTaskEvent {
            event: assistant_protocol::ChildTaskEvent::Finished {
                status: assistant_protocol::ChildTaskStatus::Completed,
                ..
            },
            ..
        }
    )));

    let listed = runtime
        .list_child_tasks(assistant_protocol::ListChildTasksRequest {
            session_id: session.session.session_id.clone(),
            parent_run_id: started.run.run_id.clone(),
        })
        .await
        .expect("list child tasks");
    assert_eq!(listed.tasks.len(), 1);
    assert_eq!(listed.tasks[0].final_text, "delegated answer");
    let queried = runtime
        .get_child_task(assistant_protocol::GetChildTaskRequest {
            session_id: session.session.session_id.clone(),
            child_task_id: child.child_task_id.clone(),
        })
        .await
        .expect("get child task");
    assert_eq!(queried.task, listed.tasks[0]);
    let other = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("other session");
    let wrong_owner = runtime
        .get_child_task(assistant_protocol::GetChildTaskRequest {
            session_id: other.session.session_id,
            child_task_id: child.child_task_id.clone(),
        })
        .await
        .expect_err("cross-session child query must be hidden");
    assert_eq!(
        wrong_owner.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::ChildTaskNotFound
    );
    runtime
        .archive_session(assistant_protocol::ArchiveSessionRequest {
            session_id: session.session.session_id.clone(),
        })
        .await
        .expect("archive session");
    assert_eq!(
        runtime
            .list_child_tasks(assistant_protocol::ListChildTasksRequest {
                session_id: session.session.session_id.clone(),
                parent_run_id: started.run.run_id,
            })
            .await
            .expect("archived child history")
            .tasks,
        listed.tasks
    );
}

#[tokio::test]
async fn child_provider_overflow_compacts_its_single_turn_and_continues() {
    let delegate_call = AssistantMessage {
        id: MessageId::new("parent-delegate-compact").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("delegate-call-compact").expect("call id"),
            name: ToolName::new("delegate_task").expect("tool name"),
            arguments: json!({
                "title": "Long child task",
                "task": "Continue after a provider overflow."
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let child_tool_call = AssistantMessage {
        id: MessageId::new("child-before-compact").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("child-probe-before-compact").expect("call id"),
            name: ToolName::new("child_probe").expect("tool name"),
            arguments: json!({"value": "before"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let summary = assistant_text("child-summary", "child work before overflow summarized");
    let child_final = assistant_text("child-after-compact", "child continued");
    let parent_final = assistant_text("parent-after-child-compact", "parent completed");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate_call)),
            ModelScript::Events(message_events(&child_tool_call)),
            ModelScript::FailEstablishment(ModelError::ContextOverflow {
                message: "fixture overflow".to_owned(),
            }),
            ModelScript::Events(message_events(&summary)),
            ModelScript::Events(message_events(&child_final)),
            ModelScript::Events(message_events(&parent_final)),
        ],
    ));
    let probe = ScriptedTool::succeed("child_probe", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(probe).expect("register probe");
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = AssistantRuntime::open(
        delegation_runtime_config(),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model.clone())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(tools.snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store.clone(),
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .expect("runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate long task".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(
        terminal.status,
        assistant_protocol::RunStatus::Completed,
        "parent error: {:?}",
        terminal.error
    );

    let recovered = store.load_runtime().await.expect("child projection");
    let child = recovered.child_tasks.first().expect("one child task");
    assert_eq!(
        child.status,
        assistant_protocol::ChildTaskStatus::Completed,
        "child error: {:?}",
        child.error
    );
    assert_eq!(child.body_generation, 2);
    let child_conversation = store
        .load_child_conversation(&session.session.session_id, &child.child_task_id)
        .await
        .expect("child conversation");
    assert!(matches!(
        child_conversation.messages.first(),
        Some(ConversationMessage::ContextSummary(summary))
            if summary.text == "child work before overflow summarized"
    ));
    assert!(matches!(
        child_conversation.messages.last(),
        Some(ConversationMessage::Assistant(message))
            if message.id.as_str() == "child-after-compact"
    ));
    let requests = model.take_requests();
    assert_eq!(requests.len(), 6);
    assert!(requests[3].tools.is_empty());
    assert_eq!(requests[3].tool_choice, ToolChoice::None);
}

#[tokio::test]
async fn child_model_failure_is_persisted_before_the_parent_receives_an_error_result() {
    let delegate_call = AssistantMessage {
        id: MessageId::new("parent-delegate-failure").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("delegate-call-failure").expect("call id"),
            name: ToolName::new("delegate_task").expect("tool name"),
            arguments: json!({
                "title": "Fail deterministically",
                "task": "This child model call fails."
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let parent_final = assistant_text("parent-after-child-failure", "handled child failure");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate_call)),
            ModelScript::FailEstablishment(ModelError::Config(
                "private child provider failure".to_owned(),
            )),
            ModelScript::Events(message_events(&parent_final)),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let workspaces = Arc::new(TestChildWorkspaceFactory::default());
    let runtime = runtime_with_store_and_child_workspaces(model, store.clone(), workspaces).await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate a failing task".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);

    let recovered = store.load_runtime().await.expect("child projection");
    let child = recovered.child_tasks.first().expect("one child task");
    assert_eq!(child.status, assistant_protocol::ChildTaskStatus::Failed);
    assert_eq!(
        child.error.as_ref().map(|error| error.code),
        Some(assistant_protocol::RuntimeErrorCode::ModelExecutionFailed)
    );
    assert!(
        !child
            .error
            .as_ref()
            .expect("safe error")
            .message
            .contains("private child provider failure")
    );

    let parent = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("parent conversation");
    let ConversationMessage::Tool(tool) = &parent.messages[2] else {
        panic!("delegate result must be a tool message");
    };
    assert_eq!(tool.result.status, agent_types::ToolResultStatus::Error);
    assert_eq!(
        parent.messages[3],
        ConversationMessage::Assistant(parent_final)
    );
}

#[tokio::test]
async fn plan_and_build_parent_agents_expose_the_same_delegate_definition() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("plan-final", "plan"))),
            ModelScript::Events(message_events(&assistant_text("build-final", "build"))),
        ],
    ));
    let runtime = runtime(model.clone());
    let plan = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("plan session");
    let build = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("build session");
    for (session_id, variant) in [
        (
            plan.session.session_id.clone(),
            assistant_protocol::AgentVariant::Plan,
        ),
        (
            build.session.session_id.clone(),
            assistant_protocol::AgentVariant::Build,
        ),
    ] {
        let started = runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant,
                session_id: session_id.clone(),
                message: "inspect definitions".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await
            .expect("run accepted");
        let terminal = wait_for_terminal(&runtime, &session_id, &started.run.run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    }
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .filter(|definition| definition.name.as_str() == "delegate_task")
            .count(),
        1
    );
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .filter(|definition| definition.name.as_str() == "update_plan")
            .count(),
        1
    );
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .filter(|definition| definition.name.as_str() == "update_goal")
            .count(),
        1
    );
}

#[tokio::test]
async fn sibling_delegations_overlap_but_respect_the_frozen_concurrency_limit() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["task-a", "task-b", "task-c"],
        Duration::from_millis(30),
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime =
        runtime_with_store(model.clone(), store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(3, 2, 1_000));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run three independent tasks".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(model.peak_children(), 2);
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert_eq!(children.len(), 3);
    assert!(
        children
            .iter()
            .all(|task| task.status == assistant_protocol::ChildTaskStatus::Completed)
    );
}

#[tokio::test]
async fn task_limit_rejects_excess_calls_without_creating_extra_child_records() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["task-a", "task-b", "task-c"],
        Duration::from_millis(1),
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(2, 2, 1_000));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "exceed the child limit".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert_eq!(children.len(), 2);
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("parent conversation");
    let results = conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(message) => Some(&message.result),
            ConversationMessage::System(_)
            | ConversationMessage::ContextSummary(_)
            | ConversationMessage::User(_)
            | ConversationMessage::Assistant(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    assert_eq!(results[2].status, agent_types::ToolResultStatus::Error);
}

#[tokio::test]
async fn child_timeout_is_failed_and_parent_can_still_summarize() {
    let model = Arc::new(
        DelegationScenarioModel::delayed(&["slow-task"], Duration::from_secs(60))
            .with_blocked_tasks(&["slow-task"]),
    );
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(1, 1, 20));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run one timed task".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].status,
        assistant_protocol::ChildTaskStatus::Failed
    );
    assert_eq!(
        children[0].error.as_ref().map(|error| error.code),
        Some(assistant_protocol::RuntimeErrorCode::Timeout)
    );
}

#[tokio::test]
async fn cancelling_one_child_does_not_cancel_its_sibling_or_parent() {
    let model = Arc::new(
        DelegationScenarioModel::delayed(&["slow-task", "fast-task"], Duration::from_millis(10))
            .with_blocked_tasks(&["slow-task"]),
    );
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run one slow and one fast task".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let children = wait_for_child_count(store.as_ref(), 2).await;
    let slow = children
        .iter()
        .find(|task| task.title == "slow-task")
        .expect("slow child");
    let cancellation = runtime
        .cancel_child_task(assistant_protocol::CancelChildTaskRequest {
            session_id: session.session.session_id.clone(),
            child_task_id: slow.child_task_id.clone(),
        })
        .await
        .expect("cancel child");
    assert!(cancellation.task.cancel_requested);

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert_eq!(
        children
            .iter()
            .find(|task| task.title == "slow-task")
            .map(|task| task.status),
        Some(assistant_protocol::ChildTaskStatus::Cancelled)
    );
    assert_eq!(
        children
            .iter()
            .find(|task| task.title == "fast-task")
            .map(|task| task.status),
        Some(assistant_protocol::ChildTaskStatus::Completed)
    );
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("parent conversation");
    let cancelled_result = conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(message) => Some(&message.result),
            ConversationMessage::System(_)
            | ConversationMessage::ContextSummary(_)
            | ConversationMessage::User(_)
            | ConversationMessage::Assistant(_) => None,
        })
        .next()
        .expect("cancelled delegate result");
    let Some(content) = cancelled_result.content.as_single_json() else {
        panic!("cancelled delegate result must preserve structured details");
    };
    assert_eq!(content["error"]["details"]["code"], "cancelled");
}

#[tokio::test]
async fn cancelling_parent_cascades_to_children_and_skips_parent_summary() {
    let model = Arc::new(
        DelegationScenarioModel::delayed(&["slow-task-a", "slow-task-b"], Duration::from_secs(60))
            .with_blocked_tasks(&["slow-task-a", "slow-task-b"]),
    );
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime =
        runtime_with_store(model.clone(), store.clone(), delegation_runtime_config()).await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run two cancellable tasks".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    wait_for_child_count(store.as_ref(), 2).await;
    runtime
        .cancel_run(assistant_protocol::CancelRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: started.run.run_id.clone(),
        })
        .await
        .expect("cancel parent");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Cancelled);
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert!(
        children
            .iter()
            .all(|task| task.status == assistant_protocol::ChildTaskStatus::Cancelled)
    );
    assert_eq!(model.parent_requests(), 1);
}

#[tokio::test]
async fn child_tool_approval_carries_child_identity_and_resolves_independently() {
    let parent_delegate = delegate_batch_message(&["child-needs-approval".to_owned()]);
    let child_tool_call = AssistantMessage {
        id: MessageId::new("child-tool-proposal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("child-probe-call").expect("tool call id"),
            name: ToolName::new("child_probe").expect("tool name"),
            arguments: json!({"value": "probe"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&parent_delegate)),
            ModelScript::Events(message_events(&child_tool_call)),
            ModelScript::Events(message_events(&assistant_text(
                "child-after-deny",
                "child handled denial",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "parent-after-child-approval",
                "parent completed",
            ))),
        ],
    ));
    let probe = ScriptedTool::succeed("child_probe", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(probe.clone()).expect("register probe");
    let runtime = runtime_with_tools(model, tools.snapshot());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate with approval".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let parent_approval = wait_for_pending_approval(&runtime, &session.session.session_id).await;
    assert_eq!(parent_approval.child_task_id, None);
    assert!(matches!(
        &parent_approval.subject,
        assistant_protocol::ToolApprovalSubject::Delegation {
            tool_name,
            title,
            task_summary,
        } if tool_name == "delegate_task"
            && title == "child-needs-approval"
            && task_summary == "child-needs-approval"
    ));
    assert!(matches!(
        &parent_approval.exact_rule_preview,
        assistant_protocol::ToolApprovalSubject::General { tool_name }
            if tool_name == "delegate_task"
    ));
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session.session.session_id.clone(),
            approval_id: parent_approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow delegation");

    let child_approval = wait_for_pending_approval(&runtime, &session.session.session_id).await;
    let child_task_id = child_approval
        .child_task_id
        .clone()
        .expect("child approval identity");
    assert_eq!(child_approval.run_id, started.run.run_id);
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session.session.session_id.clone(),
            approval_id: child_approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::Deny,
        })
        .await
        .expect("deny child tool");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert!(probe.executed_inputs().is_empty());
    let stored = runtime
        .store
        .load_runtime()
        .await
        .expect("children")
        .child_tasks;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].child_task_id, child_task_id);
}

#[tokio::test]
async fn denying_delegate_approval_does_not_create_a_child_task() {
    let parent_delegate = delegate_batch_message(&["must-not-start".to_owned()]);
    let parent_final = assistant_text("parent-after-deny", "delegation was denied");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&parent_delegate)),
            ModelScript::Events(message_events(&parent_final)),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "deny this delegation".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let approval = wait_for_pending_approval(&runtime, &session.session.session_id).await;
    assert!(matches!(
        approval.subject,
        assistant_protocol::ToolApprovalSubject::Delegation { .. }
    ));
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session.session.session_id.clone(),
            approval_id: approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::Deny,
        })
        .await
        .expect("deny delegation");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(terminal.text, "delegation was denied");
    assert!(
        store
            .load_runtime()
            .await
            .expect("runtime projection")
            .child_tasks
            .is_empty()
    );
}

#[tokio::test]
async fn child_waiting_for_approval_does_not_block_its_sibling() {
    let model = Arc::new(
        DelegationScenarioModel::delayed(&["approval-task", "fast-task"], Duration::from_millis(5))
            .with_approval_tasks(&["approval-task"]),
    );
    let probe = ScriptedTool::succeed("child_probe", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(probe.clone()).expect("register probe");
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = AssistantRuntime::open(
        delegation_runtime_config(),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(tools.snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store.clone(),
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .expect("runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate siblings".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let parent_approvals = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let approvals = runtime
                .list_pending_approvals(ListPendingApprovalsRequest {
                    session_id: session.session.session_id.clone(),
                })
                .expect("approvals")
                .approvals;
            if approvals.len() == 2 {
                return approvals;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both delegation approvals");
    assert!(
        parent_approvals
            .iter()
            .all(|approval| approval.child_task_id.is_none())
    );
    for approval in parent_approvals {
        runtime
            .decide_approval(assistant_protocol::DecideApprovalRequest {
                session_id: session.session.session_id.clone(),
                approval_id: approval.approval_id,
                decision: assistant_protocol::ApprovalDecision::AllowOnce,
            })
            .await
            .expect("allow delegation");
    }

    let child_approval = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let approvals = runtime
                .list_pending_approvals(ListPendingApprovalsRequest {
                    session_id: session.session.session_id.clone(),
                })
                .expect("approvals")
                .approvals;
            if let Some(approval) = approvals
                .into_iter()
                .find(|approval| approval.child_task_id.is_some())
            {
                return approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child approval");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let children = store.load_runtime().await.expect("children").child_tasks;
            if children.iter().any(|task| {
                task.title == "fast-task"
                    && task.status == assistant_protocol::ChildTaskStatus::Completed
            }) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fast sibling completes while approval waits");
    assert_eq!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session.session.session_id.clone(),
            })
            .expect("pending child approval")
            .approvals
            .len(),
        1
    );
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session.session.session_id.clone(),
            approval_id: child_approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::Deny,
        })
        .await
        .expect("deny child tool");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert!(probe.executed_inputs().is_empty());
}

#[tokio::test]
async fn cancelling_child_removes_its_pending_approval_without_running_the_tool() {
    let parent_delegate = delegate_batch_message(&["child-awaits-cancel".to_owned()]);
    let child_tool_call = AssistantMessage {
        id: MessageId::new("child-cancel-proposal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("child-cancel-call").expect("tool call id"),
            name: ToolName::new("child_probe").expect("tool name"),
            arguments: json!({"value": "never-run"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&parent_delegate)),
            ModelScript::Events(message_events(&child_tool_call)),
            ModelScript::Events(message_events(&assistant_text(
                "parent-after-child-cancel",
                "parent completed",
            ))),
        ],
    ));
    let probe = ScriptedTool::succeed("child_probe", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(probe.clone()).expect("register probe");
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = AssistantRuntime::open(
        delegation_runtime_config(),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(tools.snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store.clone(),
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .expect("runtime");
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    let mut events = runtime.subscribe_events();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate then cancel child".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let parent_approval = wait_for_pending_approval(&runtime, &session.session.session_id).await;
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session.session.session_id.clone(),
            approval_id: parent_approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow delegation");
    let child_approval = wait_for_pending_approval(&runtime, &session.session.session_id).await;
    let child_task_id = child_approval
        .child_task_id
        .clone()
        .expect("child approval identity");
    assert!(
        runtime
            .child_tasks
            .cancel_active(
                &session.session.session_id,
                &started.run.run_id,
                &child_task_id,
                crate::delegation::cancellation::ChildCancellationReason::Requested,
            )
            .expect("cancel child")
    );

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session.session.session_id.clone(),
            })
            .expect("approvals")
            .approvals
            .is_empty()
    );
    assert!(probe.executed_inputs().is_empty());
    let children = store.load_runtime().await.expect("children").child_tasks;
    assert_eq!(
        children[0].status,
        assistant_protocol::ChildTaskStatus::Cancelled
    );
    let mut cancellation_has_child_identity = false;
    while let Ok(event) = events.try_recv() {
        cancellation_has_child_identity |= matches!(
            event,
            assistant_protocol::RuntimeEvent::ApprovalCancelled {
                child_task_id: Some(ref observed_child_task_id),
                ..
            } if observed_child_task_id == &child_task_id
        );
    }
    assert!(cancellation_has_child_identity);
}
