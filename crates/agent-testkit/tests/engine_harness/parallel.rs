use super::*;

fn parallel_tool(name: &str, gate: ToolExecutionGate, log: &OrderLog) -> ScriptedTool {
    ScriptedTool::succeed(name, json!({"tool": name}), log.clone())
        .with_execution_mode(ToolExecutionMode::ParallelEligible)
        .with_execution_gate(gate)
}

#[tokio::test]
async fn default_serial_tools_do_not_overlap() {
    let log = OrderLog::new();
    let first_gate = ToolExecutionGate::new();
    let second_gate = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "first", json!({})),
            call("call_2", "second", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&text_message("message_final", "done"))),
        ],
    ));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("first", json!({"value": 1}), log.clone())
            .with_execution_gate(first_gate.clone()),
        ScriptedTool::succeed("second", json!({"value": 2}), log.clone())
            .with_execution_gate(second_gate.clone()),
    ]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder, authorizer),
    );

    first_gate.wait_for_entered(1).await;
    assert_eq!(second_gate.entered(), 0, "second serial tool started early");
    first_gate.release();
    second_gate.wait_for_entered(1).await;
    second_gate.release();

    let (outcome, _) = finish(execution).await;
    assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
}

#[tokio::test]
async fn explicit_parallel_group_authorizes_and_executes_concurrently_in_original_order() {
    let log = OrderLog::new();
    let execution_gate = ToolExecutionGate::new();
    let authorization_gate = AuthorizeGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "first", json!({})),
            call("call_2", "second", json!({})),
        ],
    );
    let final_message = text_message("message_final", "done");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let tools = snapshot_of(vec![
        parallel_tool("first", execution_gate.clone(), &log),
        parallel_tool("second", execution_gate.clone(), &log),
    ]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer =
        Arc::new(ScriptedAuthorizer::allow_all(log).with_gate(authorization_gate.clone()));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );

    authorization_gate.wait_for_entered(2).await;
    assert_eq!(
        execution_gate.entered(),
        0,
        "execution crossed authorization"
    );
    authorization_gate.release();
    execution_gate.wait_for_entered(2).await;
    execution_gate.release();

    let (outcome, _) = finish(execution).await;
    assert_completed(outcome, final_message);
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                success_result("call_1", json!({"tool": "first"})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                success_result("call_2", json!({"tool": "second"})),
            )),
        ]
    );
}

#[tokio::test]
async fn serial_tool_is_a_barrier_between_parallel_groups() {
    let log = OrderLog::new();
    let first_group = ToolExecutionGate::new();
    let serial = ToolExecutionGate::new();
    let second_group = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "parallel_a", json!({})),
            call("call_2", "parallel_b", json!({})),
            call("call_3", "serial", json!({})),
            call("call_4", "parallel_c", json!({})),
            call("call_5", "parallel_d", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&text_message("message_final", "done"))),
        ],
    ));
    let tools = snapshot_of(vec![
        parallel_tool("parallel_a", first_group.clone(), &log),
        parallel_tool("parallel_b", first_group.clone(), &log),
        ScriptedTool::succeed("serial", json!(null), log.clone())
            .with_execution_gate(serial.clone()),
        parallel_tool("parallel_c", second_group.clone(), &log),
        parallel_tool("parallel_d", second_group.clone(), &log),
    ]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder, authorizer),
    );

    first_group.wait_for_entered(2).await;
    assert_eq!(serial.entered(), 0);
    assert_eq!(second_group.entered(), 0);
    first_group.release();

    serial.wait_for_entered(1).await;
    assert_eq!(second_group.entered(), 0);
    serial.release();

    second_group.wait_for_entered(2).await;
    second_group.release();
    let (outcome, _) = finish(execution).await;
    assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
}

#[tokio::test]
async fn one_parallel_eligible_call_still_finishes_before_the_next_serial_call() {
    let log = OrderLog::new();
    let parallel = ToolExecutionGate::new();
    let serial = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "parallel", json!({})),
            call("call_2", "serial", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&text_message("message_final", "done"))),
        ],
    ));
    let tools = snapshot_of(vec![
        parallel_tool("parallel", parallel.clone(), &log),
        ScriptedTool::succeed("serial", json!(null), log.clone())
            .with_execution_gate(serial.clone()),
    ]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        make_input(vec![]).0,
        make_context(
            Arc::new(InMemoryRecorder::new(log.clone())),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    parallel.wait_for_entered(1).await;
    assert_eq!(serial.entered(), 0, "serial call crossed the group barrier");
    parallel.release();
    serial.wait_for_entered(1).await;
    serial.release();

    assert!(matches!(
        finish(execution).await.0,
        ExecutionOutcome::Completed { .. }
    ));
}

#[tokio::test]
async fn invalid_call_is_a_barrier_between_parallel_groups() {
    let log = OrderLog::new();
    let first_group = ToolExecutionGate::new();
    let second_group = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "parallel_a", json!({})),
            call("call_2", "parallel_b", json!({})),
            call("call_3", "missing_tool", json!({})),
            call("call_4", "parallel_c", json!({})),
            call("call_5", "parallel_d", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&text_message("message_final", "done"))),
        ],
    ));
    let tools = snapshot_of(vec![
        parallel_tool("parallel_a", first_group.clone(), &log),
        parallel_tool("parallel_b", first_group.clone(), &log),
        parallel_tool("parallel_c", second_group.clone(), &log),
        parallel_tool("parallel_d", second_group.clone(), &log),
    ]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        make_input(vec![]).0,
        make_context(
            Arc::new(InMemoryRecorder::new(log.clone())),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    first_group.wait_for_entered(2).await;
    assert_eq!(second_group.entered(), 0);
    first_group.release();
    second_group.wait_for_entered(2).await;
    second_group.release();

    assert!(matches!(
        finish(execution).await.0,
        ExecutionOutcome::Completed { .. }
    ));
}

#[tokio::test]
async fn denied_call_in_parallel_group_does_not_block_allowed_sibling() {
    let log = OrderLog::new();
    let execution_gate = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "allowed", json!({})),
            call("call_2", "denied", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&text_message("message_final", "done"))),
        ],
    ));
    let allowed = parallel_tool("allowed", execution_gate.clone(), &log);
    let denied = parallel_tool("denied", ToolExecutionGate::new(), &log);
    let tools = snapshot_of(vec![allowed, denied.clone()]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::with_decisions(
        log,
        [(
            "denied".to_owned(),
            ToolAuthorization::Deny {
                reason: "denied by test".to_owned(),
            },
        )],
    ));
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        make_input(vec![]).0,
        make_context(recorder.clone(), authorizer),
    );

    execution_gate.wait_for_entered(1).await;
    execution_gate.release();
    assert!(matches!(
        finish(execution).await.0,
        ExecutionOutcome::Completed { .. }
    ));
    assert!(denied.executed_inputs().is_empty());
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                success_result("call_1", json!({"tool": "allowed"})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "denied by test"),
            )),
        ]
    );
}

#[tokio::test]
async fn started_record_failure_in_parallel_group_blocks_every_tool_side_effect() {
    let log = OrderLog::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "first", json!({})),
            call("call_2", "second", json!({})),
        ],
    );
    let final_message = text_message("message_final", "handled start failures");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let first = ScriptedTool::succeed("first", json!(null), log.clone())
        .with_execution_mode(ToolExecutionMode::ParallelEligible);
    let second = ScriptedTool::succeed("second", json!(null), log.clone())
        .with_execution_mode(ToolExecutionMode::ParallelEligible);
    let tools = snapshot_of(vec![first.clone(), second.clone()]);
    let recorder = Arc::new(InMemoryRecorder::failing_start(log.clone()));
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        make_input(vec![]).0,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    assert_completed(finish(execution).await.0, final_message);
    assert!(first.executed_inputs().is_empty());
    assert!(second.executed_inputs().is_empty());
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                error_result("call_1", "tool execution start could not be recorded"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "tool execution start could not be recorded"),
            )),
        ]
    );
}

#[tokio::test]
async fn parallel_group_reserves_budget_before_starting_excess_calls() {
    let log = OrderLog::new();
    let execution_gate = ToolExecutionGate::new();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "first", json!({})),
            call("call_2", "second", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn))],
    ));
    let first = parallel_tool("first", execution_gate.clone(), &log);
    let second = parallel_tool("second", ToolExecutionGate::new(), &log);
    let tools = snapshot_of(vec![first, second.clone()]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model,
            tools,
            ExecutionBudget {
                max_steps: None,
                max_tool_calls: Some(1),
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );

    execution_gate.wait_for_entered(1).await;
    execution_gate.release();
    let (outcome, _) = finish(execution).await;
    assert_failed(
        outcome,
        ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 1,
        },
    );
    assert!(second.executed_inputs().is_empty());
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                success_result("call_1", json!({"tool": "first"})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "tool call budget exceeded (limit 1)"),
            )),
        ]
    );
}

#[tokio::test]
async fn cancellation_waits_for_every_parallel_tool_cleanup_and_settles_the_group() {
    let log = OrderLog::new();
    let first_entered = Arc::new(Notify::new());
    let second_entered = Arc::new(Notify::new());
    let first_cleanup = Arc::new(Notify::new());
    let second_cleanup = Arc::new(Notify::new());
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "first", json!({})),
            call("call_2", "second", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn))],
    ));
    let tools = snapshot_of(vec![
        ScriptedTool::hanging("first", log.clone())
            .with_execution_mode(ToolExecutionMode::ParallelEligible)
            .with_entered_signal(first_entered.clone())
            .with_cleanup_signal(first_cleanup.clone()),
        ScriptedTool::hanging("second", log.clone())
            .with_execution_mode(ToolExecutionMode::ParallelEligible)
            .with_entered_signal(second_entered.clone())
            .with_cleanup_signal(second_cleanup.clone()),
    ]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let AgentExecution {
        events,
        completion,
        control,
    } = execution;
    let collector = tokio::spawn(events.collect::<Vec<_>>());

    first_entered.notified().await;
    second_entered.notified().await;
    control.cancel();
    assert_cancelled(completion.await);
    first_cleanup.notified().await;
    second_cleanup.notified().await;
    let events = collector.await.expect("event collector");
    assert_lifecycle(&events);
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                error_result("call_1", "interrupted: execution cancelled"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "interrupted: execution cancelled"),
            )),
        ]
    );
}

#[tokio::test]
async fn repeated_guardrail_blocks_parallel_group_before_authorization_or_execution() {
    let log = OrderLog::new();
    let tool = ScriptedTool::succeed("repeat", json!(null), log.clone())
        .with_execution_mode(ToolExecutionMode::ParallelEligible);
    let executed = tool.clone();
    let turn = calls_message(
        "message_tools",
        vec![
            call("call_1", "repeat", json!({"same": true})),
            call("call_2", "repeat", json!({"same": true})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, _) = make_input(vec![]);
    let spec = with_guardrails(
        make_spec(model, snapshot_of(vec![tool]), ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: Some(guardrail_check(ActiveGuardrailMode::Enforce, 2)),
            consecutive_failures: None,
        },
    );

    let (outcome, _) = finish(AgentExecution::start(
        spec,
        input,
        make_context(recorder, authorizer.clone()),
    ))
    .await;
    assert_failed(
        outcome,
        ExecutionError::GuardrailTriggered {
            kind: GuardrailKind::RepeatedInvocation,
            threshold: NonZeroU32::new(2).expect("non-zero"),
        },
    );
    assert!(authorizer.observations().is_empty());
    assert!(executed.executed_inputs().is_empty());
}
