use super::*;

#[tokio::test]
async fn non_running_lifecycle_rejects_new_sessions_but_queries_remain_available() {
    let runtime = runtime(empty_model());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    for lifecycle in [RuntimeLifecycle::ShuttingDown, RuntimeLifecycle::Stopped] {
        runtime.set_lifecycle(lifecycle);
        assert!(matches!(
            runtime.create_session(CreateSessionRequest::default()).await,
            Err(RuntimeError::RuntimeNotRunning { lifecycle: actual }) if actual == lifecycle
        ));
        assert_eq!(runtime.lifecycle().expect("lifecycle"), lifecycle);
        assert!(matches!(
            runtime.submit_input(SubmitInputRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session.session.session_id.clone(),
                message: "must not start".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            }).await,
            Err(RuntimeError::RuntimeNotRunning { lifecycle: actual }) if actual == lifecycle
        ));
        assert_eq!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: session.session.session_id.clone()
                })
                .expect("query remains available")
                .session,
            session.session
        );
    }
}

#[test]
fn runtime_is_send_and_sync_without_session_tasks() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AssistantRuntime>();
}

#[tokio::test]
async fn completed_run_commits_user_before_model_and_final_assistant_once() {
    let final_message = assistant_text("assistant-final", "done");
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        final_message.clone(),
    ));
    let runtime = runtime(model.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "hello".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");
    assert_eq!(started.run.status, assistant_protocol::RunStatus::Accepted);
    assert!(started.run.run_id.as_str().starts_with("r_"));

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(terminal.text, "done");
    assert!(terminal.error.is_none());

    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        requests[0].conversation.messages.as_slice(),
        [ConversationMessage::User(_)]
    ));
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation");
    assert_eq!(conversation.messages.len(), 2);
    assert!(matches!(
        conversation.messages[0],
        ConversationMessage::User(_)
    ));
    assert_eq!(
        conversation.messages[1],
        ConversationMessage::Assistant(final_message)
    );
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone(),
            })
            .expect("session query")
            .session
            .active_run_id,
        None
    );
}

#[tokio::test]
async fn completed_model_usage_is_projected_as_a_runtime_event_and_persisted_in_conversation() {
    let usage = agent_types::TokenUsage {
        input_tokens: 1_024,
        output_tokens: 128,
        total_tokens: 1_152,
        cached_input_tokens: Some(768),
        reasoning_tokens: Some(32),
    };
    let mut final_message = assistant_text("assistant-usage", "done");
    final_message.usage = Some(usage.clone());
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        final_message,
    ));
    let runtime = runtime(model);
    let mut events = runtime.subscribe_events();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "report usage".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);

    let mut projected_usage = None;
    loop {
        let event = events.recv().await.expect("runtime event");
        match event {
            RuntimeEvent::UsageUpdated {
                run_id,
                step,
                usage,
                ..
            } if run_id == started.run.run_id => {
                projected_usage = Some((step, usage));
            }
            RuntimeEvent::RunFinished { run_id, .. } if run_id == started.run.run_id => break,
            _ => {}
        }
    }
    assert_eq!(
        projected_usage,
        Some((
            1,
            assistant_protocol::TokenUsageSnapshot {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_input_tokens: usage.cached_input_tokens,
            },
        ))
    );

    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation");
    let ConversationMessage::Assistant(message) = &conversation.messages[1] else {
        panic!("final message must be Assistant");
    };
    assert_eq!(message.usage.as_ref(), Some(&usage));
}

#[tokio::test]
async fn slow_or_dropped_event_subscribers_never_block_run_completion() {
    let final_message = assistant_text("assistant-final", "done");
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        final_message,
    ));
    let runtime = runtime_with_tools_and_capacity(model, ToolSetSnapshot::default(), 1);
    let mut lagging = runtime.subscribe_events();
    let dropped = runtime.subscribe_events();
    drop(dropped);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "hello".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert!(matches!(lagging.recv().await, Err(RecvError::Lagged(_))));
}

#[tokio::test]
async fn successful_tool_exchange_is_committed_before_the_next_model_step() {
    let log = OrderLog::new();
    let mut registry = ToolRegistry::new();
    registry
        .register(
            ScriptedTool::succeed("echo_tool", json!({"echo": "hello"}), log).with_output_chunks(
                vec![
                    ToolOutputChunk {
                        channel: AgentToolOutputChannel::Stdout,
                        delta: "hello".to_owned(),
                    },
                    ToolOutputChunk {
                        channel: AgentToolOutputChannel::Stderr,
                        delta: "warning".to_owned(),
                    },
                ],
            ),
        )
        .expect("register tool");
    let tool_message = assistant_tool_call("assistant-tools", "echo_tool");
    let final_message = assistant_text("assistant-final", "tool finished");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&tool_message)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let runtime = runtime_with_tools(model.clone(), registry.snapshot());
    let mut events = runtime.subscribe_events();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "use echo".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(terminal.text, "tool finished");
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation");
    assert_eq!(conversation.messages.len(), 4);
    conversation
        .validate_tool_exchange_pairs()
        .expect("tool exchange remains canonical");
    assert_eq!(model.take_requests().len(), 2);

    let observed = tokio::time::timeout(Duration::from_secs(1), async {
        let mut observed = Vec::new();
        loop {
            let event = events
                .recv()
                .await
                .expect("event remains in bounded buffer");
            let finished = matches!(
                &event,
                RuntimeEvent::RunFinished { run_id, .. } if run_id == &started.run.run_id
            );
            observed.push(event);
            if finished {
                return observed;
            }
        }
    })
    .await
    .expect("terminal event arrives");
    assert!(matches!(
        observed.first(),
        Some(RuntimeEvent::SessionCreated { .. })
    ));
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunAccepted { run_id, .. } if run_id == &started.run.run_id
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunStarted { run_id, .. } if run_id == &started.run.run_id
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolProposed { tool_name, .. } if tool_name == "echo_tool"
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolOutput {
            channel: assistant_protocol::ToolOutputChannel::Stdout,
            chunk,
            ..
        } if chunk == "hello"
    )));
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolOutput {
            channel: assistant_protocol::ToolOutputChannel::Stderr,
            chunk,
            ..
        } if chunk == "warning"
    )));
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::RunFinished { .. }))
            .count(),
        1
    );
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::RunFinished {
            run_id,
            status,
            error: None,
            ..
        } if run_id == &terminal.run_id && status == &terminal.status
    )));
    assert_eq!(terminal.tools.len(), 1);
    assert_eq!(terminal.tools[0].stdout, "hello");
    assert_eq!(terminal.tools[0].stderr, "warning");
    assert_eq!(
        terminal.tools[0].status,
        assistant_protocol::ToolActivityStatus::Completed
    );
}

#[tokio::test]
async fn pending_tool_exchange_is_hidden_and_a_second_input_remains_queued() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let log = OrderLog::new();
    let tool = ScriptedTool::hanging("slow_tool", log)
        .with_entered_signal(entered.clone())
        .with_cleanup_signal(cleanup.clone());
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("register tool");
    let tools = registry.snapshot();
    let tool_message = assistant_tool_call("assistant-tools", "slow_tool");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&tool_message))],
    ));
    let runtime = runtime_with_tools(model, tools);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "use the tool".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("tool entered");
    let during = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation during tool");
    assert!(matches!(
        during.messages.as_slice(),
        [ConversationMessage::User(_)]
    ));
    let queued = runtime
        .submit_input(assistant_protocol::SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "must not be appended".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("second input is queued");
    assert_eq!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .await
            .expect("conversation remains unchanged")
            .messages
            .len(),
        1
    );
    runtime
        .cancel_queued_input(assistant_protocol::CancelQueuedInputRequest {
            session_id: session.session.session_id.clone(),
            input_id: queued.input_id,
        })
        .await
        .expect("cancel queued input");

    runtime
        .cancel_run(CancelRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: started.run.run_id.clone(),
        })
        .await
        .expect("cancel active run");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("tool cleanup");
    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Cancelled);
    let completed = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("completed cancellation conversation");
    assert_eq!(completed.messages.len(), 3);
    completed
        .validate_tool_exchange_pairs()
        .expect("cancelled tool exchange remains complete");
}
