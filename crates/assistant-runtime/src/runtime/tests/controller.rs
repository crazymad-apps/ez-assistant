use super::*;
use agent_types::UserMessageOrigin;

struct FirstCallGatedModel {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelService for FirstCallGatedModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let entered = self.entered.clone();
        let release = self.release.clone();
        let message = if first {
            assistant_text("gated-source-final", "gated result")
        } else {
            assistant_text("gated-report-final", "report handled")
        };
        let events = message_events(&message);
        Box::pin(async move {
            if first {
                entered.notify_one();
                release.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(events)) as agent_model::ModelEventStream)
        })
    }
}

/// 首次调用用于挂起主控 Run，后续调用依次驱动目标 Goal 续跑、完成和代理报告。
struct GatedControllerGoalModel {
    capabilities: ModelCapabilities,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelService for GatedControllerGoalModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.clone();
        let release = self.release.clone();
        let message = match call {
            0 => assistant_text("controller-goal-source-final", "controller source finished"),
            1 => assistant_text("delegated-goal-first-final", "first delegated run finished"),
            2 => AssistantMessage {
                id: MessageId::new("delegated-goal-complete-signal").expect("message id"),
                model: ModelIdentity::new(
                    ProviderId::new("fixture").expect("provider id"),
                    "fixture-model",
                ),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId::new("delegated-goal-complete-call").expect("call id"),
                    name: ToolName::new("update_goal").expect("tool name"),
                    arguments: json!({"status": "complete", "summary": "delegated work done"}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
            3 => assistant_text("delegated-goal-completed-final", "delegated Goal completed"),
            _ => assistant_text("delegated-goal-report-final", "proxy report handled"),
        };
        let events = message_events(&message);
        Box::pin(async move {
            if call == 0 {
                entered.notify_one();
                release.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(events)) as agent_model::ModelEventStream)
        })
    }
}

#[tokio::test]
async fn controller_user_run_gets_controller_tools_but_standard_run_does_not() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "controller-final",
                "controller done",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "standard-final",
                "standard done",
            ))),
        ],
    ));
    let runtime = runtime(model.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let standard = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("standard");

    let controller_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: controller.session.session_id.clone(),
            message: "inspect sessions".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("controller input");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &controller.session.session_id,
            &controller_run.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );
    let standard_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: standard.session.session_id.clone(),
            message: "ordinary work".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("standard input");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &standard.session.session_id,
            &standard_run.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    let names = |index: usize| {
        requests[index]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
    };
    let controller_tools = names(0);
    assert!(controller_tools.contains(&"list_managed_sessions"));
    assert!(controller_tools.contains(&"set_session_proxy"));
    assert!(controller_tools.contains(&"send_session_message"));
    let standard_tools = names(1);
    assert!(!standard_tools.contains(&"list_managed_sessions"));
    assert!(!standard_tools.contains(&"set_session_proxy"));
    assert!(!standard_tools.contains(&"send_session_message"));
}

#[tokio::test]
async fn queued_delivery_is_silently_removed_when_user_takes_over() {
    let entered = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
    }));
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let controller_input = runtime
        .submit_input(SubmitInputRequest {
            session_id: controller.session.session_id.clone(),
            message: "hold controller run".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("controller input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("controller run started");

    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    let target_controller = runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    let receipt = runtime
        .controller_tool_coordinator()
        .deliver(
            &controller.session.session_id,
            &controller_input.run.run_id,
            &assistant_protocol::ToolCallId::new("controller-call-1").expect("call id"),
            &target.session.session_id,
            "delegated work".to_owned(),
            false,
        )
        .await
        .expect("delivery");
    assert_eq!(receipt.status, "accepted");
    let duplicate = runtime
        .controller_tool_coordinator()
        .deliver(
            &controller.session.session_id,
            &controller_input.run.run_id,
            &assistant_protocol::ToolCallId::new("controller-call-1").expect("call id"),
            &target.session.session_id,
            "ignored duplicate body".to_owned(),
            false,
        )
        .await
        .expect("delivery retry");
    assert_eq!(duplicate.status, "already_accepted");
    assert_eq!(duplicate.input_id, receipt.input_id);
    assert_eq!(
        crate::runtime::product::queue_snapshot(&target_controller, &Default::default())
            .expect("queued delivery")
            .items[0]
            .as_message()
            .expect("message")
            .source,
        assistant_protocol::ConversationInputSourceSnapshot::ControllerDelivery {
            controller_session_id: controller.session.session_id.clone(),
            controller_run_id: controller_input.run.run_id.clone(),
        }
    );
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: false,
        })
        .await
        .expect("queue does not block disabling proxy");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("queue does not block enabling proxy");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: false,
        })
        .await
        .expect("queue does not block disabling proxy again");

    let user = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "user takes over".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: Some(
                assistant_protocol::IdempotencyKey::new("takeover-1").expect("key"),
            ),
        })
        .await
        .expect("takeover input");
    {
        let state = target_controller.lock_state().expect("target state");
        assert!(state.proxy.is_none());
        assert!(!state.inputs.values().any(|input| {
            input.stored.cross_session.as_ref().is_some_and(|envelope| {
                matches!(
                    envelope.binding,
                    crate::CrossSessionInputBinding::ControllerDelivery { .. }
                )
            })
        }));
        assert!(
            !state
                .runs
                .values()
                .any(|run| { run.snapshot().run_id.as_str() == receipt.run_id })
        );
        assert!(state.inputs.contains_key(&user.input_id));
    }

    runtime
        .shutdown(ShutdownRuntimeRequest {})
        .await
        .expect("shutdown");
}

#[tokio::test]
async fn controller_delivery_goal_keeps_reply_route_through_continuation_and_report() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(GatedControllerGoalModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
        release: release.clone(),
        calls: AtomicUsize::new(0),
    }));
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let source = runtime
        .submit_input(SubmitInputRequest {
            session_id: controller.session.session_id.clone(),
            message: "delegate a long task".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("controller source input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("controller source started");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    let target_controller = runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    let receipt = runtime
        .controller_tool_coordinator()
        .deliver(
            &controller.session.session_id,
            &source.run.run_id,
            &assistant_protocol::ToolCallId::new("controller-goal-call").expect("call id"),
            &target.session.session_id,
            "complete the delegated long task".to_owned(),
            true,
        )
        .await
        .expect("start delegated Goal");
    let target_input_id = assistant_protocol::InputId::new(receipt.input_id).expect("input id");
    {
        let state = target_controller.lock_state().expect("target state");
        assert!(state.goal.is_some());
        let input = state.inputs.get(&target_input_id).expect("delegated input");
        assert_eq!(
            input
                .stored
                .goal_binding
                .as_ref()
                .expect("Goal binding")
                .reply_route,
            Some(crate::ReplyRoute::SessionDefault)
        );
        assert_eq!(
            input
                .stored
                .cross_session
                .as_ref()
                .expect("cross-session envelope")
                .reply_route,
            crate::ReplyRoute::SessionDefault
        );
    }

    release.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &controller.session.session_id, &source.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = false;
    runtime
        .wake_queue(target_controller.clone())
        .expect("wake delegated Goal");
    let target_run_id = assistant_protocol::RunId::new(receipt.run_id).expect("run id");
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &target_run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if target_controller
                .lock_state()
                .expect("target state")
                .goal
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("delegated Goal completed");
    {
        let state = target_controller.lock_state().expect("target state");
        let bindings = state
            .inputs
            .values()
            .filter_map(|input| input.stored.goal_binding.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(
            bindings.len(),
            1,
            "delegated Goal continues inside its original Input and Run"
        );
        assert!(
            bindings
                .iter()
                .all(|binding| { binding.reply_route == Some(crate::ReplyRoute::SessionDefault) })
        );
    }
    let controller_session = runtime.session_for_test(&controller.session.session_id);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let report_route = controller_session
                .lock_state()
                .expect("controller state")
                .inputs
                .values()
                .find_map(|input| {
                    let envelope = input.stored.cross_session.as_ref()?;
                    matches!(
                        envelope.binding,
                        crate::CrossSessionInputBinding::ProxyReport { .. }
                    )
                    .then_some(envelope.reply_route.clone())
                });
            if let Some(route) = report_route {
                assert_eq!(route, crate::ReplyRoute::SessionDefault);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("delegated Goal proxy report");
}

#[tokio::test]
async fn proxy_reports_only_the_run_that_drains_the_queue_and_gets_no_controller_tools() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "target-first-final",
                "first result",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "target-last-final",
                "last result",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "controller-report-final",
                "report acknowledged",
            ))),
        ],
    ));
    let runtime = runtime(model.clone());
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let target_controller = runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    let first = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "first queued task".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("first input");
    let last = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "last queued task".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("last input");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy while queue exists");
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = false;
    runtime
        .wake_queue(target_controller.clone())
        .expect("wake target queue");

    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &first.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &last.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let controller_session = runtime.session_for_test(&controller.session.session_id);
    let report_run_id = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(run_id) = controller_session
                .lock_state()
                .expect("controller state")
                .inputs
                .values()
                .find_map(|input| {
                    match input
                        .stored
                        .cross_session
                        .as_ref()
                        .map(|envelope| &envelope.binding)
                    {
                        Some(crate::CrossSessionInputBinding::ProxyReport {
                            source_run_id,
                            ..
                        }) => {
                            assert_eq!(source_run_id, &last.run.run_id);
                            Some(input.latest_run_id.clone())
                        }
                        _ => None,
                    }
                })
            {
                break run_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("proxy report accepted");
    assert_eq!(
        wait_for_terminal(&runtime, &controller.session.session_id, &report_run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let report_inputs = controller_session
        .lock_state()
        .expect("controller state")
        .inputs
        .values()
        .filter(|input| {
            input.stored.cross_session.as_ref().is_some_and(|envelope| {
                matches!(
                    envelope.binding,
                    crate::CrossSessionInputBinding::ProxyReport { .. }
                )
            })
        })
        .count();
    assert_eq!(report_inputs, 1);

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    let report_tools = requests[2]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(!report_tools.contains(&"list_managed_sessions"));
    assert!(!report_tools.contains(&"set_session_proxy"));
    assert!(!report_tools.contains(&"send_session_message"));
}

#[tokio::test]
async fn recovery_interrupts_source_and_accepts_proxy_report_before_registry_publish() {
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let first_runtime = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(std::num::NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    let controller = first_runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = first_runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let target_controller = first_runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    let accepted = first_runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "work interrupted by crash".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("target input");
    first_runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    let (input_id, message) = {
        let state = target_controller.lock_state().expect("target state");
        let input = state.inputs.get(&accepted.input_id).expect("stored input");
        (
            input.stored.input_id.clone(),
            input.stored.queued_message.clone().expect("queued message"),
        )
    };
    store
        .commit_user_message(crate::UserMessageCommit {
            operation_id: "recovery-start".to_owned(),
            input_id,
            run_id: accepted.run.run_id.clone(),
            session_id: target.session.session_id.clone(),
            message: Some(message),
            reasoning_effort: None,
            created_at_ms: 100,
        })
        .await
        .expect("persist running source");
    drop(first_runtime);

    let recovered_runtime = runtime_with_store(
        empty_model(),
        store,
        RuntimeConfig::new(std::num::NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    assert_eq!(
        recovered_runtime
            .get_run(assistant_protocol::GetRunRequest {
                session_id: target.session.session_id.clone(),
                run_id: accepted.run.run_id.clone(),
            })
            .await
            .expect("recovered source Run")
            .run
            .status,
        assistant_protocol::RunStatus::Interrupted
    );
    let recovered_controller = recovered_runtime.session_for_test(&controller.session.session_id);
    let report = recovered_controller
        .lock_state()
        .expect("controller state")
        .inputs
        .values()
        .find_map(|input| {
            match input
                .stored
                .cross_session
                .as_ref()
                .map(|envelope| &envelope.binding)
            {
                Some(crate::CrossSessionInputBinding::ProxyReport {
                    source_run_id,
                    source_run_status,
                    ..
                }) => Some((source_run_id.clone(), *source_run_status)),
                _ => None,
            }
        })
        .expect("recovery proxy report");
    assert_eq!(report.0, accepted.run.run_id);
    assert_eq!(report.1, assistant_protocol::RunStatus::Interrupted);
}

#[tokio::test]
async fn enabling_proxy_during_an_active_run_reports_that_run_at_settlement() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(FirstCallGatedModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
        release: release.clone(),
        calls: AtomicUsize::new(0),
    }));
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let source = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "start before proxy".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("source input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("source model entered");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy during run");
    release.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &source.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let controller_session = runtime.session_for_test(&controller.session.session_id);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if controller_session
                .lock_state()
                .expect("controller state")
                .inputs
                .values()
                .any(|input| {
                    input.stored.cross_session.as_ref().is_some_and(|envelope| {
                        matches!(
                            &envelope.binding,
                            crate::CrossSessionInputBinding::ProxyReport {
                                source_run_id,
                                ..
                            } if source_run_id == &source.run.run_id
                        )
                    })
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("active run report accepted");
}

#[tokio::test]
async fn failed_prestart_run_reports_a_stable_failure_when_it_drains_the_queue() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::FailEstablishment(ModelError::Config(
            "private provider failure".to_owned(),
        ))],
    )));
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let target_controller = runtime.session_for_test(&target.session.session_id);
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = true;
    let source = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "fail safely".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("source input");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    target_controller
        .lock_state()
        .expect("target state")
        .queue_paused_by_user = false;
    runtime.wake_queue(target_controller).expect("wake queue");
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &source.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    let controller_session = runtime.session_for_test(&controller.session.session_id);
    let (status, report_run_id) =
        wait_for_proxy_report_status(&controller_session, &source.run.run_id).await;
    assert_eq!(status, assistant_protocol::RunStatus::Failed);
    wait_for_terminal(&runtime, &controller.session.session_id, &report_run_id).await;
    let report_text = controller_session
        .conversation_snapshot()
        .expect("controller conversation")
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(message) if message.origin == UserMessageOrigin::Runtime => {
                message.parts.iter().find_map(|part| match part {
                    UserPart::Text(part) => Some(part.text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("report text");
    assert!(!report_text.contains("private provider failure"));
}

#[tokio::test]
async fn disabling_proxy_during_an_active_run_suppresses_its_report() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(FirstCallGatedModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
        release: release.clone(),
        calls: AtomicUsize::new(0),
    }));
    let _controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let source = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "toggle proxy during work".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("source input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("source entered");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: false,
        })
        .await
        .expect("disable proxy");
    release.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &source.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert!(
        !runtime
            .sessions
            .read()
            .expect("sessions")
            .values()
            .any(|session| session
                .lock_state()
                .expect("session state")
                .inputs
                .values()
                .any(
                    |input| input.stored.cross_session.as_ref().is_some_and(|envelope| {
                        matches!(
                            envelope.binding,
                            crate::CrossSessionInputBinding::ProxyReport { .. }
                        )
                    })
                ))
    );
}

#[tokio::test]
async fn cancelled_run_reports_cancelled_after_proxy_is_enabled_mid_run() {
    let entered = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
    }));
    let controller = runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("controller");
    let target = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("target");
    let source = runtime
        .submit_input(SubmitInputRequest {
            session_id: target.session.session_id.clone(),
            message: "cancel managed work".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("source input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("source entered");
    runtime
        .set_session_proxy(assistant_protocol::SetSessionProxyRequest {
            session_id: target.session.session_id.clone(),
            enabled: true,
        })
        .await
        .expect("enable proxy");
    runtime
        .interrupt_run(InterruptRunRequest {
            session_id: target.session.session_id.clone(),
            run_id: source.run.run_id.clone(),
        })
        .await
        .expect("interrupt source");
    assert_eq!(
        wait_for_terminal(&runtime, &target.session.session_id, &source.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    let controller_session = runtime.session_for_test(&controller.session.session_id);
    let (status, _) = wait_for_proxy_report_status(&controller_session, &source.run.run_id).await;
    assert_eq!(status, assistant_protocol::RunStatus::Cancelled);
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown");
}

async fn wait_for_proxy_report_status(
    controller: &SessionController,
    source_run_id: &RunId,
) -> (assistant_protocol::RunStatus, RunId) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(status) = controller
                .lock_state()
                .expect("controller state")
                .inputs
                .values()
                .find_map(|input| {
                    match input
                        .stored
                        .cross_session
                        .as_ref()
                        .map(|envelope| &envelope.binding)
                    {
                        Some(crate::CrossSessionInputBinding::ProxyReport {
                            source_run_id: candidate,
                            source_run_status,
                            ..
                        }) if candidate == source_run_id => {
                            Some((*source_run_status, input.latest_run_id.clone()))
                        }
                        _ => None,
                    }
                })
            {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("proxy report accepted")
}
