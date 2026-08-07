use super::*;

#[tokio::test]
async fn tool_failure_feeds_error_result_and_continues() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "explode", json!({}))]);
    let turn2 = text_message("message_2", "The tool failed; recovered.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::failing("explode", "boom", log.clone())]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    // 工具失败不是执行错误：错误 ToolResult 回喂模型，循环继续到完成。
    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert!(events.contains(&AgentEvent::ToolCompleted {
        call_id: call_id("call_1"),
        status: ToolCompletionStatus::Failed,
    }));
    let deltas = recorder.deltas();
    assert_eq!(deltas.len(), 2);
    let ConversationDelta::Tool(message) = &deltas[1] else {
        panic!("second delta must be the tool result, got {deltas:?}");
    };
    assert_eq!(message.result.status, ToolResultStatus::Error);
    let ToolResultContent::Text(content) = &message.result.content else {
        panic!("error result must carry model-readable text");
    };
    assert!(content.contains("boom"), "unexpected content: {content}");
    // 错误结果回填进下一轮请求。
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.messages.last(),
        Some(&ConversationMessage::Tool(message.clone()))
    );
}

#[tokio::test]
async fn unknown_tool_name_feeds_error_result() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "missing_tool", json!({}))]);
    let turn2 = text_message("message_2", "No such tool; moving on.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "get_date",
        json!(null),
        log.clone(),
    )]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert!(events.contains(&AgentEvent::ToolCompleted {
        call_id: call_id("call_1"),
        status: ToolCompletionStatus::Failed,
    }));
    let deltas = recorder.deltas();
    let ConversationDelta::Tool(message) = &deltas[1] else {
        panic!("second delta must be the tool result, got {deltas:?}");
    };
    assert_eq!(
        message.result,
        error_result("call_1", "unknown tool: `missing_tool`")
    );
    // 未知名在 Dispatcher 处短路：从未进入任何工具实现。
    assert!(
        !log.entries()
            .iter()
            .any(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
    );
    assert!(
        !log.entries()
            .iter()
            .any(|entry| matches!(entry, LogEntry::Authorize { .. }))
    );
}

#[tokio::test]
async fn mixed_valid_and_invalid_batch_authorizes_only_valid_items_in_order() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "read_file", json!({"path": "a.txt"})),
            call("call_2", "missing_tool", json!({})),
            call("call_3", "write_file", json!({"path": "b.txt"})),
        ],
    );
    let turn2 = text_message("message_2", "The valid calls completed.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("read_file", json!({"content": "a"}), log.clone()),
        ScriptedTool::succeed("write_file", json!({"written": true}), log.clone()),
    ]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer.clone()),
    );
    let (outcome, _) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "read_file".to_owned(),
                batch_size: 3,
            },
            LogEntry::ToolExecute {
                name: "read_file".to_owned(),
            },
            LogEntry::Authorize {
                name: "write_file".to_owned(),
                batch_size: 3,
            },
            LogEntry::ToolExecute {
                name: "write_file".to_owned(),
            },
            LogEntry::RecordTool,
        ]
    );
    assert_eq!(authorizer.observations().len(), 2);
    let deltas = recorder.deltas();
    assert_eq!(deltas.len(), 4);
    let results = deltas[1..]
        .iter()
        .map(|delta| match delta {
            ConversationDelta::Tool(message) => message.result.clone(),
            other => panic!("expected tool delta, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        vec![
            success_result("call_1", json!({"content": "a"})),
            error_result("call_2", "unknown tool: `missing_tool`"),
            success_result("call_3", json!({"written": true})),
        ]
    );
}

#[tokio::test]
async fn authorizer_and_executor_observe_the_same_resolved_arguments() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![call("call_1", "read_file", json!({"path": "relative.txt"}))],
    );
    let turn2 = text_message("message_2", "Resolved input was shared.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let resolved_arguments = json!({"path": "/session/relative.txt", "limit": 200});
    let tool = ScriptedTool::succeed("read_file", json!({"content": "ok"}), log.clone())
        .with_resolved_input(resolved_arguments.clone());
    let tool_observer = tool.clone();
    let tools = snapshot_of(vec![tool]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder, authorizer.clone()),
    );
    let (outcome, _) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    let observations = authorizer.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].resolved_arguments, resolved_arguments);
    assert_eq!(tool_observer.executed_inputs(), vec![resolved_arguments]);
}

#[tokio::test]
async fn repeated_observe_emits_once_and_keeps_executing() {
    let log = OrderLog::new();
    let calls = (1..=4)
        .map(|index| call(&format!("call_{index}"), "read", json!({"path": "a.txt"})))
        .collect();
    let turn1 = calls_message("message_1", calls);
    let turn2 = text_message("message_2", "All repeated reads completed.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read",
        json!({"content": "a"}),
        log.clone(),
    )]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(guardrail_check(ActiveGuardrailMode::Observe, 2)),
            consecutive_failures: None,
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::GuardrailTriggered { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        vec![AgentEvent::GuardrailTriggered {
            kind: GuardrailKind::RepeatedInvocation,
            mode: ActiveGuardrailMode::Observe,
            threshold: NonZeroU32::new(2).expect("non-zero threshold"),
            observed: 2,
            call_id: call_id("call_2"),
        }]
    );
    assert_eq!(
        log.entries()
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
            .count(),
        4
    );
    assert_eq!(recorder.deltas().len(), 5);
}

#[tokio::test]
async fn repeated_sequence_is_preserved_across_model_steps() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![call("call_1", "read", json!({"path": "a.txt"}))],
    );
    let turn2 = calls_message(
        "message_2",
        vec![call("call_2", "read", json!({"path": "a.txt"}))],
    );
    let turn3 = text_message("message_3", "The cross-step repetition was observed.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
            ModelScript::Events(message_events(&turn3)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read",
        json!(null),
        log.clone(),
    )]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(guardrail_check(ActiveGuardrailMode::Observe, 2)),
            consecutive_failures: None,
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder,
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn3));
    assert!(events.contains(&AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::RepeatedInvocation,
        mode: ActiveGuardrailMode::Observe,
        threshold: NonZeroU32::new(2).expect("non-zero threshold"),
        observed: 2,
        call_id: call_id("call_2"),
    }));
}

#[tokio::test]
async fn invalid_item_resets_repeated_invocation_sequence() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "read", json!({"path": "a.txt"})),
            call("call_2", "missing", json!({})),
            call("call_3", "read", json!({"path": "a.txt"})),
        ],
    );
    let turn2 = text_message("message_2", "The invalid item broke the sequence.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read",
        json!(null),
        log.clone(),
    )]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(guardrail_check(ActiveGuardrailMode::Observe, 2)),
            consecutive_failures: None,
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder,
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::GuardrailTriggered { .. }))
    );
    assert_eq!(
        log.entries()
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn repeated_enforce_stops_before_triggering_invocation_and_settles_batch() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        (1..=3)
            .map(|index| call(&format!("call_{index}"), "read", json!({"path": "a.txt"})))
            .collect(),
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read",
        json!(null),
        log.clone(),
    )]);
    let threshold = NonZeroU32::new(2).expect("non-zero threshold");
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Enforce,
                threshold,
            }),
            consecutive_failures: None,
        },
    );
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::GuardrailTriggered {
            kind: GuardrailKind::RepeatedInvocation,
            threshold,
        })
    );
    assert!(events.contains(&AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::RepeatedInvocation,
        mode: ActiveGuardrailMode::Enforce,
        threshold,
        observed: 2,
        call_id: call_id("call_2"),
    }));
    assert!(!events.contains(&AgentEvent::ToolStarted {
        call_id: call_id("call_2"),
    }));
    assert!(!events.contains(&AgentEvent::ToolStarted {
        call_id: call_id("call_3"),
    }));
    assert_eq!(
        log.entries()
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
            .count(),
        1
    );
    let expected_message = "guardrail enforced: RepeatedInvocation reached threshold 2";
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_1",
                success_result("call_1", json!(null))
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
                error_result("call_2", expected_message),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_3",
                error_result("call_3", expected_message),
            )),
        ]
    );
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn consecutive_failures_observe_rearms_after_success() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "fail", json!({"n": 1})),
            call("call_2", "fail", json!({"n": 2})),
            call("call_3", "fail", json!({"n": 3})),
            call("call_4", "success", json!({})),
            call("call_5", "fail", json!({"n": 5})),
            call("call_6", "fail", json!({"n": 6})),
        ],
    );
    let turn2 = text_message("message_2", "Observed failures and continued.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![
        ScriptedTool::failing("fail", "boom", log.clone()),
        ScriptedTool::succeed("success", json!({"ok": true}), log.clone()),
    ]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(guardrail_check(ActiveGuardrailMode::Observe, 2)),
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder,
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    let triggers = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::GuardrailTriggered { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(triggers, vec![call_id("call_2"), call_id("call_6")]);
    assert_eq!(
        log.entries()
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
            .count(),
        6
    );
}

#[tokio::test]
async fn shell_nonzero_success_resets_consecutive_failures() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "fail", json!({"n": 1})),
            call("call_2", "shell", json!({"command": "exit 7"})),
            call("call_3", "fail", json!({"n": 3})),
            call("call_4", "fail", json!({"n": 4})),
        ],
    );
    let turn2 = text_message(
        "message_2",
        "Nonzero exit remained a successful tool result.",
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let shell = Arc::new(FakeShellTool::new([FakeShellScript {
        chunks: vec![],
        completion: FakeShellCompletion::Exit { exit_code: Some(7) },
    }]));
    let mut registry = ToolRegistry::new();
    registry
        .register(ScriptedTool::failing("fail", "boom", log.clone()))
        .expect("register failing tool");
    registry
        .register(ShellExecTool::new(
            shell,
            SessionPathResolver::new(
                AbsolutePath::new(std::env::temp_dir()).expect("absolute temp directory"),
            ),
            ShellExecToolConfig::new(
                Duration::from_secs(120),
                Duration::from_secs(600),
                NonZeroU64::new(1024).expect("non-zero"),
            )
            .expect("valid shell config"),
        ))
        .expect("register shell tool");
    let spec = with_guardrails(
        make_spec(model, registry.snapshot(), ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(guardrail_check(ActiveGuardrailMode::Observe, 2)),
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(recorder, Arc::new(ScriptedAuthorizer::allow_all(log))),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    let triggers = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::GuardrailTriggered { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(triggers, vec![call_id("call_4")]);
}

#[tokio::test]
async fn consecutive_failure_sequence_is_preserved_across_model_steps() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "fail", json!({"n": 1}))]);
    let turn2 = calls_message("message_2", vec![call("call_2", "fail", json!({"n": 2}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::failing("fail", "boom", log.clone())]);
    let threshold = NonZeroU32::new(2).expect("non-zero threshold");
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Enforce,
                threshold,
            }),
        },
    );
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::GuardrailTriggered {
            kind: GuardrailKind::ConsecutiveFailures,
            threshold,
        })
    );
    assert!(events.contains(&AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::ConsecutiveFailures,
        mode: ActiveGuardrailMode::Enforce,
        threshold,
        observed: 2,
        call_id: call_id("call_2"),
    }));
    assert_eq!(recorder.deltas().len(), 4);
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn invalid_and_deny_count_as_failures_before_enforce() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "missing", json!({})),
            call("call_2", "denied", json!({})),
            call("call_3", "never", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::with_decisions(
        log.clone(),
        [(
            "denied".to_owned(),
            ToolAuthorization::Deny {
                reason: "denied by test".to_owned(),
            },
        )],
    ));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("denied", json!(null), log.clone()),
        ScriptedTool::succeed("never", json!(null), log.clone()),
    ]);
    let threshold = NonZeroU32::new(2).expect("non-zero threshold");
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(GuardrailCheckConfig {
                mode: ActiveGuardrailMode::Enforce,
                threshold,
            }),
        },
    );
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(recorder.clone(), authorizer.clone()),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::GuardrailTriggered {
            kind: GuardrailKind::ConsecutiveFailures,
            threshold,
        })
    );
    assert!(events.contains(&AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::ConsecutiveFailures,
        mode: ActiveGuardrailMode::Enforce,
        threshold,
        observed: 2,
        call_id: call_id("call_2"),
    }));
    assert_eq!(authorizer.observations().len(), 1);
    assert_eq!(authorizer.observations()[0].call_id, call_id("call_2"));
    assert!(
        !log.entries()
            .iter()
            .any(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
    );
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_1",
                error_result("call_1", "unknown tool: `missing`"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
                error_result("call_2", "denied by test"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_3",
                error_result(
                    "call_3",
                    "guardrail enforced: ConsecutiveFailures reached threshold 2",
                ),
            )),
        ]
    );
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn recorder_complete_failure_takes_priority_over_guardrail_terminal() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "read", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::failing_at(2, log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read",
        json!(null),
        log.clone(),
    )]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(guardrail_check(ActiveGuardrailMode::Enforce, 1)),
            consecutive_failures: None,
        },
    );
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        spec,
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::Record(RecordError {
            message: "injected record failure at call 2".to_owned(),
        }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::GuardrailTriggered {
            kind: GuardrailKind::RepeatedInvocation,
            ..
        }
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ExecutionFailed {
            error: ExecutionError::Record(_),
            ..
        })
    ));
    assert!(recorder.deltas().is_empty());
    assert_eq!(recorder.pending_exchanges().len(), 1);
    assert_eq!(
        log.entries(),
        vec![LogEntry::RecordAssistant, LogEntry::RecordTool]
    );
}
