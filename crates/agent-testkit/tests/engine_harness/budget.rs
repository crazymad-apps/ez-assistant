use super::*;

#[tokio::test]
async fn max_steps_zero_fails_before_any_model_call() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&text_message(
            "message_1",
            "never reached",
        )))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model.clone(),
            ToolSetSnapshot::default(),
            ExecutionBudget {
                max_steps: Some(0),
                max_tool_calls: None,
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::BudgetExceeded {
            kind: BudgetKind::Steps,
            limit: 0,
        },
    );
    // Some(0)：一次模型调用都不发生（无 StepStarted）。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::Steps,
                    limit: 0,
                },
                dropped_events: 0,
            },
        ]
    );
    assert!(model.take_requests().is_empty());
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn max_steps_exactly_at_limit_completes() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "Done in two turns.");
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
        // 脚本恰需两个模型 Turn，上限恰为 2：预检不拦截，正常完成。
        make_spec(
            model.clone(),
            tools,
            ExecutionBudget {
                max_steps: Some(2),
                max_tool_calls: None,
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_completed(outcome, turn2);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ExecutionCompleted { .. })
    ));
    assert_eq!(model.take_requests().len(), 2);
}

#[tokio::test]
async fn max_steps_exceeded_fails_at_next_precheck() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "never requested");
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
        json!({"date": "2026-07-27"}),
        log.clone(),
    )]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model.clone(),
            tools,
            ExecutionBudget {
                max_steps: Some(1),
                max_tool_calls: None,
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::BudgetExceeded {
            kind: BudgetKind::Steps,
            limit: 1,
        },
    );
    // 第一轮完整处理后，第二轮 max_steps 预检先于 Context Evaluator 受控终止
    // （无 StepStarted{2}）。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolStarted {
                step: 1,
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::Steps,
                    limit: 1,
                },
                dropped_events: 0,
            },
        ]
    );
    // 第二个脚本从未被请求；第一轮的 Assistant 与 ToolResult 已配对落账。
    assert_eq!(model.take_requests().len(), 1);
    assert_eq!(recorder.deltas().len(), 2);
    assert!(log.entries().contains(&LogEntry::ToolExecute {
        name: "get_date".to_owned(),
    }));
}

#[tokio::test]
async fn max_tool_calls_zero_settles_batch_without_dispatch() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
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
        make_spec(
            model,
            tools,
            ExecutionBudget {
                max_steps: None,
                max_tool_calls: Some(0),
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 0,
        },
    );
    // dispatch 前预检：调用被结算预算超额错误，工具未执行，整批落账后受控终止。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "get_date".to_owned(),
                batch_size: 1,
            },
            LogEntry::RecordTool,
        ]
    );
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                error_result("call_1", "tool call budget exceeded (limit 0)"),
            )),
        ]
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    limit: 0,
                },
                dropped_events: 0,
            },
        ]
    );
}

#[tokio::test]
async fn max_tool_calls_exactly_at_limit_completes() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "One call is enough.");
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
        // 恰一次 dispatch，上限恰为 1：正常完成。
        make_spec(
            model.clone(),
            tools,
            ExecutionBudget {
                max_steps: None,
                max_tool_calls: Some(1),
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, _) = finish(execution).await;

    assert_completed(outcome, turn2);
    assert_eq!(model.take_requests().len(), 2);
}

#[tokio::test]
async fn max_tool_calls_crossed_within_batch_settles_rest() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "get_date", json!({})),
            call("call_2", "get_time", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("get_date", json!({"date": "2026-07-27"}), log.clone()),
        ScriptedTool::succeed("get_time", json!({"time": "10:00"}), log.clone()),
    ]);
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
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 1,
        },
    );
    // 同批跨越上限：call_1 已 dispatch；call_2 过了授权闸后在 dispatch 前预检处
    // 被结算预算超额错误（未执行），整批落账后受控终止。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "get_date".to_owned(),
                batch_size: 2,
            },
            LogEntry::ToolExecute {
                name: "get_date".to_owned(),
            },
            LogEntry::Authorize {
                name: "get_time".to_owned(),
                batch_size: 2,
            },
            LogEntry::RecordTool,
        ]
    );
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                success_result("call_1", json!({"date": "2026-07-27"})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "tool call budget exceeded (limit 1)"),
            )),
        ]
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_2", "get_time", json!({})),
            },
            AgentEvent::ToolStarted {
                step: 1,
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_2"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    limit: 1,
                },
                dropped_events: 0,
            },
        ]
    );
}

#[tokio::test]
async fn deny_does_not_count_toward_tool_call_budget() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "write_file", json!({"path": "b.txt"})),
            call("call_2", "read_file", json!({"path": "a.txt"})),
        ],
    );
    let turn2 = text_message("message_2", "Deny consumed no budget.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::with_decisions(
        log.clone(),
        [(
            "write_file".to_owned(),
            ToolAuthorization::Deny {
                reason: "no writes".to_owned(),
            },
        )],
    ));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("write_file", json!(null), log.clone()),
        ScriptedTool::succeed("read_file", json!({"content": "a"}), log.clone()),
    ]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        // 预算为"实际 dispatch 数"：Deny 不计入，read_file 用掉唯一的 1 次额度。
        make_spec(
            model.clone(),
            tools,
            ExecutionBudget {
                max_steps: None,
                max_tool_calls: Some(1),
            },
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, _) = finish(execution).await;

    assert_completed(outcome, turn2);
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "write_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::Authorize {
                name: "read_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::ToolExecute {
                name: "read_file".to_owned(),
            },
            LogEntry::RecordTool,
        ]
    );
}
