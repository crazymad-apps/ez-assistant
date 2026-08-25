use super::*;

#[tokio::test]
async fn cancel_during_model_stream_converges_to_cancelled() {
    let log = OrderLog::new();
    let model = Arc::new(PausedModel {
        capabilities: capabilities(),
    });
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model,
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let AgentExecution {
        mut events,
        completion,
        control,
    } = execution;

    // 等到正文 delta 到达（引擎确实在模型流中），再经 ExecutionControl 取消。
    let mut prefix = Vec::new();
    loop {
        let event = events.next().await.expect("event stream open");
        let is_text_delta = matches!(event, AgentEvent::TextDelta { .. });
        prefix.push(event);
        if is_text_delta {
            break;
        }
    }
    control.cancel();

    let outcome = completion.await;
    assert_cancelled(outcome);
    let rest: Vec<AgentEvent> = events.collect().await;
    let events = [prefix, rest].concat();
    assert_lifecycle(&events);
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::TextDelta {
                step: 1,
                id: part_id("text_1"),
                delta: "partial".to_owned(),
            },
            AgentEvent::ExecutionCancelled { dropped_events: 0 },
        ]
    );
    // 模型流中取消：无落账、无授权、无工具执行。
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn cancel_during_tool_execution_settles_rest_of_batch() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "fast", json!({})),
            call("call_2", "slow", json!({})),
            call("call_3", "never", json!({})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let slow_entered = Arc::new(Notify::new());
    let slow_cleanup_completed = Arc::new(Notify::new());
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("fast", json!({"ok": true}), log.clone()),
        ScriptedTool::hanging("slow", log.clone())
            .with_entered_signal(slow_entered.clone())
            .with_cleanup_signal(slow_cleanup_completed.clone()),
        ScriptedTool::succeed("never", json!(null), log.clone()),
    ]);
    let (input, user_input) = make_input(vec![]);
    let spec = with_guardrails(
        make_spec(model, tools, ExecutionBudget::default()),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(guardrail_check(ActiveGuardrailMode::Enforce, 1)),
        },
    );
    let execution = AgentExecution::start(spec, input, make_context(recorder.clone(), authorizer));
    let AgentExecution {
        mut events,
        completion,
        control,
    } = execution;

    // 等到第二个工具开始执行且确实进入 execute（挂起中），再取消。
    let mut prefix = Vec::new();
    loop {
        let event = events.next().await.expect("event stream open");
        let hit = matches!(&event, AgentEvent::ToolStarted { call_id: id, .. } if *id == call_id("call_2"));
        prefix.push(event);
        if hit {
            break;
        }
    }
    slow_entered.notified().await;
    control.cancel();

    let outcome = completion.await;
    assert_cancelled(outcome);
    // Engine 只有在 cancellation-aware tool 完成清理并返回后才能解析取消终态。
    slow_cleanup_completed.notified().await;
    let rest: Vec<AgentEvent> = events.collect().await;
    let events = [prefix, rest].concat();
    assert_lifecycle(&events);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::GuardrailTriggered { .. })),
        "cancellation must not count as a consecutive failure"
    );
    // 取消收敛：call_1 已成功；call_2（执行中清理完成）与 call_3（未到达）补记
    // interrupted 错误 ToolResult；整批 Recorder 成功后再按原顺序发布 ToolCompleted，
    // 唯一终态为 ExecutionCancelled。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "fast", json!({})),
            },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_2", "slow", json!({})),
            },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_3", "never", json!({})),
            },
            AgentEvent::ToolStarted {
                step: 1,
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolStarted {
                step: 1,
                call_id: call_id("call_2"),
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
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_3"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ExecutionCancelled { dropped_events: 0 },
        ]
    );
    // 顺序日志：fast 执行完成，slow 收到取消并完成清理，never 未授权未执行；
    // 清理完成后才原子 complete 整批结果。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "fast".to_owned(),
                batch_size: 3,
            },
            LogEntry::ToolExecute {
                name: "fast".to_owned(),
            },
            LogEntry::Authorize {
                name: "slow".to_owned(),
                batch_size: 3,
            },
            LogEntry::ToolExecute {
                name: "slow".to_owned(),
            },
            LogEntry::ToolCleanup {
                name: "slow".to_owned(),
            },
            LogEntry::RecordTool,
        ]
    );
    // 落账：Assistant + 三个 ToolMessage（toolmsg_1..=3 确定性序号），配对完整。
    let deltas = recorder.deltas();
    assert_eq!(
        deltas,
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_1",
                success_result("call_1", json!({"ok": true})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
                error_result("call_2", "interrupted: execution cancelled"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_3",
                error_result("call_3", "interrupted: execution cancelled"),
            )),
        ]
    );
    // 取消后新 ConversationSnapshot（输入 + 落账增量）Tool Call/Result 配对完整。
    assert_tool_pairing(&reconstruct(&user_input, &deltas));
}

#[tokio::test]
async fn cancel_during_authorize_hang_settles_batch() {
    let log = OrderLog::new();
    let gate = AuthorizeGate::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "read_file", json!({"path": "a.txt"})),
            call("call_2", "write_file", json!({"path": "b.txt"})),
        ],
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()).with_gate(gate.clone()));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("read_file", json!(null), log.clone()),
        ScriptedTool::succeed("write_file", json!(null), log.clone()),
    ]);
    let (input, user_input) = make_input(vec![]);
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

    // 授权闸已挂起（第一次 authorize 进入），取消；闸门不放行（授权等待中的取消 race）。
    gate.wait_entered().await;
    control.cancel();

    let (outcome, events) = {
        let collector = tokio::spawn(events.collect::<Vec<_>>());
        let outcome = completion.await;
        let events = collector.await.expect("event collection task panicked");
        (outcome, events)
    };
    assert_cancelled(outcome);
    assert_lifecycle(&events);
    // 两个已宣告调用均未结算：取消收敛各补 interrupted 错误 ToolResult。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "read_file", json!({"path": "a.txt"})),
            },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_2", "write_file", json!({"path": "b.txt"})),
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_2"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ExecutionCancelled { dropped_events: 0 },
        ]
    );
    // 只有一次 authorize 尝试（第二个调用未到达授权闸），无任何工具执行。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "read_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::RecordTool,
        ]
    );
    let deltas = recorder.deltas();
    assert_eq!(
        deltas,
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_1",
                error_result("call_1", "interrupted: execution cancelled"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
                error_result("call_2", "interrupted: execution cancelled"),
            )),
        ]
    );
    assert_tool_pairing(&reconstruct(&user_input, &deltas));
}

// ---------- 订阅断开 ----------

#[tokio::test]
async fn dropped_completion_receiver_still_finishes_execution() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "Receiverless completion.");
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
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let AgentExecution {
        events, completion, ..
    } = execution;
    // drop 完成接收端：执行照常收敛，终态事件照常发出。
    drop(completion);

    let events: Vec<AgentEvent> = events.collect().await;
    assert_lifecycle(&events);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ExecutionCompleted { .. })
    ));
    // 工具照常执行、Assistant 与 ToolResult 照常落账。
    assert_eq!(recorder.deltas().len(), 2);
    assert!(log.entries().contains(&LogEntry::ToolExecute {
        name: "get_date".to_owned(),
    }));
}

#[tokio::test]
async fn dropped_event_stream_still_resolves_completion() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "No subscriber at all.");
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
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let AgentExecution {
        events, completion, ..
    } = execution;
    // drop 事件订阅：执行与完成结果不受影响。
    drop(events);

    let outcome = completion.await;
    assert!(matches!(outcome, ExecutionOutcome::Completed { .. }));
    assert_eq!(recorder.deltas().len(), 2);
}
