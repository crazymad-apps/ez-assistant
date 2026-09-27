//! F01 通过正式 Runtime 验证输入、工具和请求重试的边界。
use super::*;

fn unavailable() -> ModelScript {
    ModelScript::FailEstablishment(ModelError::Unavailable {
        message: "private overload detail".into(),
        status: Some(503),
        retry_after_ms: None,
    })
}

async fn submit(runtime: &AssistantRuntime, session_id: &SessionId) -> RunId {
    runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "complete once".into(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap()
        .run
        .run_id
}

#[tokio::test]
async fn default_retry_recovers_and_preserves_one_user_input() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            unavailable(),
            ModelScript::Events(message_events(&assistant_text("final", "recovered"))),
        ],
    ));
    let runtime = runtime(model.clone());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run_id = submit(&runtime, &session_id).await;
    // 产品默认首次等待一秒；本测试覆盖真实默认值，超时只约束验收而不修改策略。
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })
                .await
                .unwrap()
                .run;
            if run.status.is_terminal() {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert!(terminal.model_request.is_none());
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    let conversation = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert_eq!(
        conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ConversationMessage::User(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn retry_after_tool_execution_does_not_replay_tool_or_user() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_tool_call(
                "tool-step",
                "echo_tool",
            ))),
            unavailable(),
            ModelScript::Events(message_events(&assistant_text("final", "done"))),
        ],
    ));
    let tool = ScriptedTool::succeed("echo_tool", json!({"ok": true}), OrderLog::new());
    let mut tools = ToolRegistry::new();
    tools.register(tool.clone()).unwrap();
    let runtime = runtime_with_tools(model.clone(), tools.snapshot());
    runtime.config_registry.replace_document_for_test(&format!("{TEST_CONFIG}\n[runtime.model_retry]\nretry_on=['unavailable']\ndelays_ms=[1]\nmax_retry_after_ms=10\n"));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &session_id).await;
    let run_id = submit(&runtime, &session_id).await;
    let terminal = wait_for_terminal(&runtime, &session_id, &run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    assert_eq!(tool.executed_inputs().len(), 1);
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1], requests[2]);
    let conversation = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert_eq!(
        conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ConversationMessage::User(_)))
            .count(),
        1
    );
    assert_eq!(
        conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ConversationMessage::Tool(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn retry_wait_is_queryable_and_cancellation_prevents_next_request() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [unavailable()],
    ));
    let runtime = runtime(model.clone());
    runtime.config_registry.replace_document_for_test(&format!("{TEST_CONFIG}\n[runtime.model_retry]\nretry_on=['unavailable']\ndelays_ms=[60000]\nmax_retry_after_ms=60000\n"));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let mut events = runtime.subscribe_events();
    let run_id = submit(&runtime, &session_id).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                events.recv().await.unwrap(),
                RuntimeEvent::ModelRetryScheduled { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    let view = runtime
        .get_run(GetRunRequest {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        })
        .await
        .unwrap()
        .run;
    assert_eq!(
        view.model_request,
        Some(assistant_protocol::ModelRequestStatus::RetryWaiting {
            next_attempt: 2,
            delay_ms: 60000
        })
    );
    runtime
        .cancel_run(assistant_protocol::CancelRunRequest {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        })
        .await
        .unwrap();
    let terminal = wait_for_terminal(&runtime, &session_id, &run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Cancelled);
    assert!(terminal.model_request.is_none());
    assert_eq!(model.take_requests().len(), 1);
}

#[tokio::test]
async fn explicit_disable_and_auth_failure_never_retry() {
    for (section, error) in [
        (
            "\n[runtime.model_retry]\nenabled=false\n",
            ModelError::Unavailable {
                message: "private".into(),
                status: Some(503),
                retry_after_ms: None,
            },
        ),
        ("", ModelError::Auth("private".into())),
    ] {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [ModelScript::FailEstablishment(error)],
        ));
        let runtime = runtime(model.clone());
        runtime
            .config_registry
            .replace_document_for_test(&format!("{TEST_CONFIG}{section}"));
        let session_id = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let run_id = submit(&runtime, &session_id).await;
        let terminal = wait_for_terminal(&runtime, &session_id, &run_id).await;
        assert_eq!(terminal.status, assistant_protocol::RunStatus::Failed);
        assert!(!terminal.error.unwrap().message.contains("private"));
        assert_eq!(model.take_requests().len(), 1);
    }
}
