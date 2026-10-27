use super::*;

#[tokio::test]
async fn session_view_does_not_reread_main_history_for_each_child() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["first", "second", "third"],
        Duration::ZERO,
    ));
    let store = Arc::new(super::store::FaultInjectingStore::healthy());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(3, 3, 1000));
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &id).await;
    submit_completed_turn(&runtime, &id, "delegate").await;
    store.main_history_loads.store(0, Ordering::SeqCst);
    store.child_history_loads.store(0, Ordering::SeqCst);
    let view = runtime
        .get_session_view(GetSessionViewRequest { session_id: id })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(view.child_tasks.len(), 3);
    assert!(
        view.child_tasks
            .iter()
            .all(|child| child.task.status.is_terminal() && child.task.final_text.is_empty())
    );
    assert!(
        store.main_history_loads.load(Ordering::SeqCst) <= 2,
        "one main Run read and one child directory read must suffice for the whole tree"
    );
    assert_eq!(
        store.child_history_loads.load(Ordering::SeqCst),
        0,
        "terminal child summaries must use the parent directory and usage snapshots"
    );
    let detail = runtime
        .get_child_task(assistant_protocol::GetChildTaskRequest {
            session_id: view.session.session_id.clone(),
            child_task_id: view.child_tasks[0].task.child_task_id.clone(),
        })
        .await
        .unwrap();
    assert!(
        !detail.task.final_text.is_empty(),
        "opening the child must still load its final response"
    );
    // child 正文提交同样属于父 Session 的观察范围，不能返回混合快照。
    let events = runtime.event_sender.clone();
    let owner = assistant_protocol::ConversationOwner::ChildTask {
        session_id: view.session.session_id.clone(),
        child_task_id: view.child_tasks[0].task.child_task_id.clone(),
    };
    *store.window_read_hook.lock().unwrap() = Some(Box::new(move || {
        let _ = events.send(RuntimeEvent::ConversationCommitted {
            owner: owner.clone(),
            generation: 1,
        });
    }));
    assert!(matches!(
        runtime
            .get_session_view(GetSessionViewRequest {
                session_id: view.session.session_id.clone(),
            })
            .await,
        Err(RuntimeError::SnapshotBusy)
    ));
    *store.window_read_hook.lock().unwrap() = None;
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn fork_child_history_is_owner_scoped_readonly_and_survives_source_deletion() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["fork child"],
        Duration::ZERO,
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime =
        runtime_with_store(model.clone(), store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(1, 1, 1000));
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &source).await;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            session_id: source.clone(),
            message: "delegate once".to_owned(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let child = store
        .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
            session_id: source.clone(),
        })
        .await
        .unwrap()
        .facts
        .child_tasks
        .remove(0);
    let body = store
        .load_child_conversation(&source, &child.child_task_id)
        .await
        .unwrap();
    let mut previous = source.clone();
    let mut targets = Vec::new();
    for _ in 0..2 {
        let fork = runtime
            .fork_session(assistant_protocol::ForkSessionRequest {
                session_id: previous,
                fork_point: assistant_protocol::MessageId::new("parent-delegate-batch").unwrap(),
                expected_generation: 1,
            })
            .await
            .unwrap()
            .session
            .session_id;
        let view = runtime
            .get_session_view(GetSessionViewRequest {
                session_id: fork.clone(),
            })
            .await
            .unwrap()
            .snapshot
            .value;
        assert_eq!(view.child_tasks.len(), 1);
        assert!(view.child_tasks[0].task.inherited);
        assert!(!view.child_tasks[0].task.body_unavailable);
        assert!(!view.child_tasks[0].can_cancel);
        let listed = runtime
            .list_child_tasks(assistant_protocol::ListChildTasksRequest {
                session_id: fork.clone(),
                parent_run_id: submitted.run.run_id.clone(),
            })
            .await
            .unwrap()
            .tasks;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].inherited);
        let child_view = runtime
            .get_child_task_view(assistant_protocol::GetChildTaskViewRequest {
                session_id: fork.clone(),
                child_task_id: child.child_task_id.clone(),
            })
            .await
            .unwrap()
            .snapshot
            .value;
        assert_eq!(child_view.task.task.final_text, "result for fork child");
        assert_eq!(child_view.task.task.session_id, fork);
        assert_eq!(
            store
                .load_child_conversation(&fork, &child.child_task_id)
                .await
                .unwrap(),
            body
        );
        assert!(
            runtime
                .cancel_child_task(assistant_protocol::CancelChildTaskRequest {
                    session_id: fork.clone(),
                    child_task_id: child.child_task_id.clone(),
                })
                .await
                .is_err()
        );
        assert!(!store.contains_child_control(&child.child_task_id).unwrap());
        previous = fork.clone();
        targets.push(fork);
    }
    assert_eq!(
        model.parent_requests(),
        2,
        "Fork/read never invokes the model"
    );
    for deleted in [&source, &targets[0]] {
        let confirmation = runtime
            .prepare_delete_session(assistant_protocol::PrepareDeleteSessionRequest {
                session_id: deleted.clone(),
            })
            .await
            .unwrap();
        runtime
            .delete_session(assistant_protocol::DeleteSessionRequest {
                session_id: deleted.clone(),
                confirmation_token: confirmation.confirmation_token,
            })
            .await
            .unwrap();
    }
    drop(runtime);
    let runtime =
        runtime_with_store(model.clone(), store.clone(), delegation_runtime_config()).await;
    let remaining = &targets[1];
    let view = runtime
        .get_child_task_view(assistant_protocol::GetChildTaskViewRequest {
            session_id: remaining.clone(),
            child_task_id: child.child_task_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(view.task.task.session_id, *remaining);
    assert_eq!(view.task.task.final_text, "result for fork child");
    assert_eq!(
        store
            .load_child_conversation(remaining, &child.child_task_id)
            .await
            .unwrap(),
        body
    );
    assert!(
        store
            .load_child_conversation(&source, &child.child_task_id)
            .await
            .is_err()
    );
    assert_eq!(model.parent_requests(), 2);
}

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
                        UserPart::FileReferences(_)
                        | UserPart::Injected(_)
                        | UserPart::InternalContext(_)
                        | UserPart::QuotedText(_) => unreachable!(),
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
                    purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
) -> Vec<crate::StoredChildTaskHistory> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let tasks = store
                .load_runtime()
                .await
                .expect("child projection")
                .child_history
                .into_iter()
                .map(|(_, task)| task)
                .collect::<Vec<_>>();
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
    isolated_child_result(false).await;
}

#[tokio::test]
async fn child_stream_retry_preserves_isolation_and_returns_only_its_final_result() {
    isolated_child_result(true).await;
}

fn watch_child_commit_barrier(
    store: &super::store::FaultInjectingStore,
    session: Arc<crate::session::SessionController>,
) -> Arc<Mutex<Vec<&'static str>>> {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let stages = observed.clone();
    *store.child_write_hook.lock().unwrap() = Some(Box::new(move |stage| {
        assert!(
            futures_util::FutureExt::now_or_never(session.mutation()).is_none(),
            "child {stage} must hold the parent snapshot barrier"
        );
        stages.lock().unwrap().push(stage);
    }));
    observed
}

async fn isolated_child_result(interrupt_child: bool) {
    let delegate_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
    let mut script = vec![ModelScript::Events(message_events(&delegate_call))];
    if interrupt_child {
        script.push(ModelScript::Events(vec![
            agent_model::ModelEvent::TurnFailed {
                error: ModelError::Transport {
                    kind: agent_model::ModelTransportErrorKind::Interrupted,
                    message: "child connection lost".into(),
                },
            },
        ]));
    }
    script.extend([
        ModelScript::Events(message_events(&child_final)),
        ModelScript::Events(message_events(&parent_final)),
    ]);
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        script,
    ));
    let store = Arc::new(super::store::FaultInjectingStore::healthy());
    let workspaces = Arc::new(TestChildWorkspaceFactory::default());
    let runtime =
        runtime_with_store_and_child_workspaces(model.clone(), store.clone(), workspaces.clone())
            .await;
    let mut config = config_with_delegation_limits(8, 4, 300_000, 40, 100, 1_024);
    if interrupt_child {
        config.push_str("\n[runtime.model_retry]\nretry_on=['connection']\ndelays_ms=[1]\nmax_retry_after_ms=10\n");
    }
    runtime.config_registry.replace_document_for_test(&config);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let stages = watch_child_commit_barrier(
        &store,
        runtime.session(&session.session.session_id).await.unwrap(),
    );
    let mut events = runtime.subscribe_events();
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate this".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    let committed_stages = stages.lock().unwrap().clone();
    for stage in ["create", "start", "settle"] {
        assert!(
            committed_stages.contains(&stage),
            "missing child {stage} commit"
        );
    }
    *store.child_write_hook.lock().unwrap() = None;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(terminal.text, "parent received delegated answer");

    let mut requests = model.take_requests();
    if interrupt_child {
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[1], requests[2]);
        requests.remove(2);
    }
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
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "load_skill")
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
    assert_eq!(recovered.child_history.len(), 1);
    let child = &recovered.child_history[0].1;
    assert!(
        !store.contains_child_control(&child.child_task_id).unwrap(),
        "terminal child control is released after reliable history"
    );
    assert!(recovered.child_tasks.is_empty());
    let window = store
        .load_conversation_window(crate::ConversationWindowRequest {
            owner: assistant_protocol::ConversationOwner::ChildTask {
                session_id: session.session.session_id.clone(),
                child_task_id: child.child_task_id.clone(),
            },
            generation: child.body_generation,
            end: None,
            limit: 30,
        })
        .await
        .unwrap();
    let facts = window.history.unwrap();
    assert_eq!(facts.child_tasks, vec![child.clone()]);
    assert_eq!(facts.message_origins.len(), 2);
    assert!(
        facts
            .message_origins
            .iter()
            .all(|origin| origin.run_id.is_none()
                && origin.input_id.is_none()
                && origin.attempt.is_none())
    );
    let impact = store
        .inspect_session_deletion(&session.session.session_id)
        .await
        .unwrap();
    assert_eq!(
        impact.child_task_count, 1,
        "released child remains in deletion impact"
    );
    assert_eq!(
        child.status,
        Some(assistant_protocol::ChildTaskStatus::Completed),
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
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::ModelAttemptStarted { .. }))
            .count(),
        2,
        "child attempts must not be attributed to the parent"
    );

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
            event: assistant_protocol::ChildTaskEvent::Started { started_at_ms },
            ..
        } if started_at_ms.is_some() && *started_at_ms == child.started_at_ms
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
    let mut stale = runtime
        .child_tasks
        .get(&session.session.session_id, &child.child_task_id)
        .unwrap()
        .unwrap();
    stale.status = assistant_protocol::ChildTaskStatus::Failed;
    stale.title = "stale execution observation".into();
    stale.finished_at_ms = Some(1);
    stale.error = Some(assistant_protocol::RuntimeErrorInfo {
        code: assistant_protocol::RuntimeErrorCode::InvalidRequest,
        message: "stale observation".into(),
    });
    runtime
        .child_tasks
        .remove_session(&session.session.session_id)
        .unwrap();
    runtime.child_tasks.upsert(stale).unwrap();
    assert_eq!(
        runtime
            .get_child_task(assistant_protocol::GetChildTaskRequest {
                session_id: session.session.session_id.clone(),
                child_task_id: child.child_task_id.clone(),
            })
            .await
            .unwrap()
            .task,
        queried.task
    );
    assert_eq!(
        runtime
            .cancel_child_task(assistant_protocol::CancelChildTaskRequest {
                session_id: session.session.session_id.clone(),
                child_task_id: child.child_task_id.clone(),
            })
            .await
            .unwrap()
            .task,
        queried.task
    );
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
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
    let restarted = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(256).unwrap()),
    )
    .await;
    let restored = restarted
        .get_child_task(assistant_protocol::GetChildTaskRequest {
            session_id: session.session.session_id.clone(),
            child_task_id: child.child_task_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(restored.task, queried.task);
    let cancelled = restarted
        .cancel_child_task(assistant_protocol::CancelChildTaskRequest {
            session_id: session.session.session_id.clone(),
            child_task_id: child.child_task_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(cancelled.task, restored.task);
    assert!(
        !store
            .load_runtime()
            .await
            .unwrap()
            .child_tasks
            .iter()
            .any(|control| control.child_task_id == child.child_task_id)
    );
    assert!(
        model.take_requests().is_empty(),
        "history queries never resume the child"
    );
    if interrupt_child {
        store
            .delete_session(crate::SessionDeletion {
                session_id: session.session.session_id.clone(),
                operation_id: "delete-child-history".into(),
                expected_impact: impact,
            })
            .await
            .unwrap();
    } else {
        restarted
            .restore_session(assistant_protocol::RestoreSessionRequest {
                session_id: session.session.session_id.clone(),
            })
            .await
            .unwrap();
        let view = restarted
            .get_session_view(GetSessionViewRequest {
                session_id: session.session.session_id.clone(),
            })
            .await
            .unwrap();
        restarted
            .clear_session(assistant_protocol::ClearSessionRequest {
                session_id: session.session.session_id.clone(),
                operation_id: assistant_protocol::IdempotencyKey::new("clear-child-history")
                    .unwrap(),
                expected_generation: view.snapshot.value.conversation_generation,
            })
            .await
            .unwrap();
    }
    assert!(
        store
            .load_child_conversation(&session.session.session_id, &child.child_task_id)
            .await
            .is_err()
    );
    assert!(store.load_runtime().await.unwrap().child_history.is_empty());
}

#[tokio::test]
async fn child_compaction_preserves_dynamic_load_skill_request_and_continues() {
    child_compaction_projection_case("success").await;
}

#[tokio::test]
async fn child_compaction_registry_failure_does_not_write_stale_child_or_parent_terminal() {
    child_compaction_projection_case("registry").await;
}

#[tokio::test]
async fn child_compaction_provider_failure_retains_http_status_in_child_history() {
    child_compaction_projection_case("provider400").await;
}

async fn child_compaction_projection_case(case: &str) {
    let child_task = "Continue after a provider overflow with all prior details. ".repeat(80);
    let delegate_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
                "task": child_task,
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let child_tool_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("child-before-compact").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("child-load-skill-before-compact").expect("call id"),
            name: ToolName::new("load_skill").expect("tool name"),
            arguments: json!({"name": "review"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let child_summary = assistant_text("child-compact-summary", "child history summarized");
    let child_final = assistant_text("child-after-compact", "child completed");
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
            if case == "provider400" {
                ModelScript::FailEstablishment(ModelError::Provider {
                    message: "private-child-provider-marker".into(),
                    status: Some(400),
                })
            } else {
                ModelScript::Events(message_events(&child_summary))
            },
            ModelScript::Events(message_events(&child_final)),
            ModelScript::Events(message_events(&parent_final)),
        ],
    ));
    let store = Arc::new(super::store::FaultInjectingStore::healthy());
    let mut runtime = runtime_with_store_and_child_workspaces(
        model.clone(),
        store.clone(),
        Arc::new(TestChildWorkspaceFactory::default()),
    )
    .await;
    runtime.skill_package_source = Arc::new(super::sessions::StaticSkillPackageSource);
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let stages = watch_child_commit_barrier(
        &store,
        runtime.session(&session.session.session_id).await.unwrap(),
    );
    if case == "registry" {
        let registry = runtime.child_tasks.clone();
        *store.context_write_hook.lock().unwrap() = Some(Box::new(move |target, committed| {
            if committed.is_some()
                && matches!(target, crate::ContextReplacementTarget::ChildTask { .. })
            {
                registry.poison_projection_for_test();
            }
        }));
    }
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate long task".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    if case == "registry" {
        let owner = runtime.session(&session.session.session_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if owner.lock_state().unwrap().is_faulted {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // 等待原执行完全退出，避免仅检查故障标记而遗漏较晚的陈旧终态写入。
        runtime.tasks.close();
        assert!(runtime.tasks.wait_or_abort(Duration::from_secs(2)).await);
        let recovered = store
            .load_session_state(&session.session.session_id)
            .await
            .unwrap()
            .state;
        assert_eq!(
            recovered.runs[0].status,
            assistant_protocol::RunStatus::Running
        );
        assert_eq!(
            recovered.child_tasks[0].status,
            assistant_protocol::ChildTaskStatus::Running
        );
        assert_eq!(recovered.child_tasks[0].body_generation, 2);
        let history = store
            .load_child_conversation(
                &session.session.session_id,
                &recovered.child_tasks[0].child_task_id,
            )
            .await
            .unwrap();
        assert!(
            history
                .messages
                .iter()
                .any(|m| matches!(m, ConversationMessage::ContextSummary(_)))
        );
        assert_eq!(
            model.take_requests().len(),
            4,
            "no child or parent continuation after projection failure"
        );
        return;
    }
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    let committed_stages = stages.lock().unwrap().clone();
    let expected_stages: &[&str] = if case == "provider400" {
        &["create", "start", "settle"]
    } else {
        &["create", "start", "settle", "replace"]
    };
    for stage in expected_stages {
        assert!(
            committed_stages.contains(stage),
            "missing child {stage} commit"
        );
    }
    *store.child_write_hook.lock().unwrap() = None;
    assert_eq!(
        terminal.status,
        assistant_protocol::RunStatus::Completed,
        "parent error: {:?}",
        terminal.error
    );

    let recovered = store.load_runtime().await.expect("child projection");
    assert_eq!(
        store
            .selection_control_counts(&session.session.session_id)
            .unwrap(),
        (0, 0)
    );
    assert_eq!(recovered.skill_activations.len(), 1);
    let child = &recovered.child_history.first().expect("one child task").1;
    if case == "provider400" {
        assert_eq!(
            child.status,
            Some(assistant_protocol::ChildTaskStatus::Failed)
        );
        let error = child.error.as_ref().unwrap();
        assert_eq!(
            error.code,
            assistant_protocol::RuntimeErrorCode::ContextCompactionFailed
        );
        assert!(error.message.contains("status=400"));
        assert!(!error.message.contains("private-child-provider-marker"));
        assert_eq!(child.body_generation, 1);
        let requests = model.take_requests();
        assert_eq!(
            requests.len(),
            5,
            "400 is not retried and child does not continue"
        );
        assert!(requests[3].tools.is_empty());
        return;
    }
    assert_eq!(
        child.status,
        Some(assistant_protocol::ChildTaskStatus::Completed),
        "child error: {:?}",
        child.error
    );
    assert!(child.error.is_none());
    assert_eq!(child.body_generation, 2);
    assert_eq!(child.context_window_tokens, Some(8192));
    let child_conversation = store
        .load_child_conversation(&session.session.session_id, &child.child_task_id)
        .await
        .expect("child conversation");
    assert!(
        child_conversation
            .messages
            .iter()
            .any(|message| matches!(message, ConversationMessage::ContextSummary(_)))
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 6);
    let overflow_request = &requests[2];
    let summary_request = &requests[3];
    assert!(
        overflow_request
            .tools
            .iter()
            .any(|definition| definition.name.as_str() == "load_skill")
    );
    assert_eq!(summary_request.system, overflow_request.system);
    assert!(summary_request.tools.is_empty());
    assert_eq!(requests[4].tools, overflow_request.tools);
    assert_eq!(summary_request.reasoning, overflow_request.reasoning);
    assert_eq!(
        summary_request.provider_options,
        overflow_request.provider_options
    );
    assert_eq!(summary_request.tool_choice, ToolChoice::None);
    assert_eq!(
        &summary_request.conversation.messages[..overflow_request.conversation.messages.len()],
        overflow_request.conversation.messages.as_slice()
    );
    assert_eq!(
        summary_request.conversation.messages.len(),
        overflow_request.conversation.messages.len() + 1
    );
}

#[tokio::test]
async fn child_model_failure_is_persisted_before_the_parent_receives_an_error_result() {
    let delegate_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate a failing task".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);

    let recovered = store.load_runtime().await.expect("child projection");
    let child = &recovered.child_history.first().expect("one child task").1;
    assert_eq!(
        child.status,
        Some(assistant_protocol::ChildTaskStatus::Failed)
    );
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
            .submit_started_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant,
                session_id: session_id.clone(),
                message: "inspect definitions".to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run three independent tasks".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(model.peak_children(), 2);
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 3);
    assert!(
        children
            .iter()
            .all(|task| task.status == Some(assistant_protocol::ChildTaskStatus::Completed))
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "exceed the child limit".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run one timed task".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].status,
        Some(assistant_protocol::ChildTaskStatus::Failed)
    );
    assert_eq!(
        children[0].error.as_ref().map(|error| error.code),
        Some(assistant_protocol::RuntimeErrorCode::Timeout)
    );
}

#[tokio::test]
async fn child_timeout_disabled_allows_task_to_finish() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["slow-task"],
        Duration::from_millis(40),
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(model, store.clone(), delegation_runtime_config()).await;
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(1, 1, 0));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run one timed task".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].status,
        Some(assistant_protocol::ChildTaskStatus::Completed)
    );
    assert_eq!(children[0].error.as_ref().map(|error| error.code), None);
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run one slow and one fast task".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(
        children
            .iter()
            .find(|task| task.title == "slow-task")
            .and_then(|task| task.status),
        Some(assistant_protocol::ChildTaskStatus::Cancelled)
    );
    assert_eq!(
        children
            .iter()
            .find(|task| task.title == "fast-task")
            .and_then(|task| task.status),
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "run two cancellable tasks".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert!(
        children
            .iter()
            .all(|task| task.status == Some(assistant_protocol::ChildTaskStatus::Cancelled))
    );
    assert_eq!(model.parent_requests(), 1);
}

#[tokio::test]
async fn child_tool_approval_carries_child_identity_and_resolves_independently() {
    let parent_delegate = delegate_batch_message(&["child-needs-approval".to_owned()]);
    let child_tool_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate with approval".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].child_task_id, child_task_id);
}

#[tokio::test]
async fn child_tool_reads_the_parent_sessions_new_approval_mode() {
    let parent_delegate = delegate_batch_message(&["child-uses-current-mode".to_owned()]);
    let child_tool_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("child-current-mode-tool").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("child-current-mode-call").expect("tool call id"),
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
                "child-current-mode-final",
                "child completed",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "parent-current-mode-final",
                "parent completed",
            ))),
        ],
    ));
    let probe = ScriptedTool::succeed("child_probe", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(probe.clone()).expect("register probe");
    let runtime = runtime_with_tools(model, tools.snapshot());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "delegate after changing approval mode".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run accepted")
        .run;
    let parent_approval = wait_for_pending_approval(&runtime, &session_id).await;
    assert!(parent_approval.child_task_id.is_none());

    set_auto_approval(&runtime, &session_id).await;
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: parent_approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow the already-pending delegation");

    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(probe.executed_inputs().len(), 1);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .await
            .expect("child used automatic approval")
            .approvals
            .is_empty()
    );
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "deny this delegation".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
            .child_history
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
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate siblings".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
                .await
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
                .await
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
            let children = store
                .load_runtime()
                .await
                .expect("children")
                .child_history
                .into_iter()
                .map(|(_, task)| task)
                .collect::<Vec<_>>();
            if children.iter().any(|task| {
                task.title == "fast-task"
                    && task.status == Some(assistant_protocol::ChildTaskStatus::Completed)
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
            .await
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
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;
    let mut events = runtime.subscribe_events();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "delegate then cancel child".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
            .await
            .expect("approvals")
            .approvals
            .is_empty()
    );
    assert!(probe.executed_inputs().is_empty());
    let children = store
        .load_runtime()
        .await
        .expect("children")
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(
        children[0].status,
        Some(assistant_protocol::ChildTaskStatus::Cancelled)
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

#[tokio::test]
async fn sibling_model_routing_is_distinct_from_parent_and_each_other() {
    let model = Arc::new(DelegationScenarioModel::delayed(
        &["routing-child-a", "routing-child-b"],
        Duration::from_millis(10),
    ));
    let factory = Arc::new(RoutingModelFactory::new(model));
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(2, 2, 1000));
    let session = runtime
        .create_session(Default::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &session).await;
    submit_completed_turn(&runtime, &session, "two independent tasks").await;
    let children = runtime
        .store
        .load_runtime()
        .await
        .unwrap()
        .child_history
        .into_iter()
        .map(|(_, task)| task)
        .collect::<Vec<_>>();
    assert_eq!(children.len(), 2);
    let calls = factory.calls.lock().unwrap();
    let parent = super::super::model::conversation_routing_id(
        &assistant_protocol::ConversationOwner::MainSession {
            session_id: session.clone(),
        },
    );
    assert_eq!(calls.first().unwrap().0, parent);
    assert_eq!(calls.last().unwrap().0, parent);
    let child_routes: Vec<_> = children
        .iter()
        .map(|child| {
            super::super::model::conversation_routing_id(
                &assistant_protocol::ConversationOwner::ChildTask {
                    session_id: session.clone(),
                    child_task_id: child.child_task_id.clone(),
                },
            )
        })
        .collect();
    assert_ne!(child_routes[0], child_routes[1]);
    for route in child_routes {
        assert_ne!(route, parent);
        assert!(calls.iter().any(|call| call.0 == route));
    }
}

#[tokio::test]
async fn mcp_management_tools_are_parent_only_and_not_inherited_by_child() {
    let mut call = assistant_tool_call("delegate-management", "delegate_task");
    let AssistantPart::ToolCall(part) = &mut call.parts[0] else {
        unreachable!()
    };
    part.arguments = json!({"title":"child test", "task":"return text"});
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&call)),
            ModelScript::Events(message_events(&assistant_text("child-done", "done"))),
            ModelScript::Events(message_events(&assistant_text("parent-done", "done"))),
        ],
    ));
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(crate::mcp::MissingMcpConfigSource),
        Arc::new(crate::mcp::UnavailableMcpConnectionFactory),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime
        .config_registry
        .replace_document_for_test(&config_with_delegation(1, 1, 1000));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &session).await;
    submit_completed_turn(&runtime, &session, "delegate once").await;
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    for name in ["get_mcp_configuration", "configure_mcp", "refresh_mcp"] {
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name.as_str() == name)
        );
        assert!(
            !requests[1]
                .tools
                .iter()
                .any(|tool| tool.name.as_str() == name)
        );
    }
}
