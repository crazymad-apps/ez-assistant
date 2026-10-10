use super::*;
use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};

/// 测试私有工具：取消后先等待清理闸门，再返回确定的业务结果；不模拟真实文件 I/O。
struct ReturnedAfterCancelTool {
    name: &'static str,
    mode: ToolExecutionMode,
    entered: Arc<Notify>,
    cleanup_started: Arc<Notify>,
    cleanup_release: CancellationToken,
    returned: Arc<Notify>,
    result: (ToolResultStatus, Value),
}

impl Tool for ReturnedAfterCancelTool {
    type Input = Value;
    type ResolvedInput = Value;
    type Output = (ToolResultStatus, Value);

    fn name(&self) -> ToolName {
        tool_name(self.name)
    }

    fn description(&self) -> String {
        "Return a known result after cancellation cleanup.".to_owned()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        self.mode
    }

    fn resolve(&self, input: Value) -> Result<ToolResolution<Value>, ToolError> {
        Ok(ToolResolution::general(input))
    }

    fn execute<'a>(
        &'a self,
        _input: Value,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.entered.notify_one();
            context.cancellation.cancelled().await;
            self.cleanup_started.notify_one();
            self.cleanup_release.cancelled().await;
            self.returned.notify_one();
            Ok(self.result.clone())
        })
    }

    fn output_status(output: &Self::Output) -> ToolResultStatus {
        output.0.clone()
    }

    fn encode_output(output: Self::Output) -> Result<ToolResultContent, String> {
        Ok(ToolResultContent::json(output.1))
    }
}

// 同一确定性场景覆盖串行/并行、成功/部分失败和 Recorder 故障；只用信号同步清理顺序。
async fn assert_returned_cancellation_result(
    mode: ToolExecutionMode,
    status: ToolResultStatus,
    record_failure: bool,
) {
    let log = OrderLog::new();
    let entered = Arc::new(Notify::new());
    let cleanup_started = Arc::new(Notify::new());
    let cleanup_release = CancellationToken::new();
    let returned = Arc::new(Notify::new());
    let payload = if status == ToolResultStatus::Success {
        json!({"committed": true})
    } else {
        json!({"files": [
            {"path": "a.txt", "status": "success", "replacements": 1},
            {"path": "b.txt", "status": "not_executed", "reason": "cancelled"}
        ]})
    };
    let mut registry = ToolRegistry::new();
    registry
        .register(ReturnedAfterCancelTool {
            name: "partial",
            mode,
            entered: entered.clone(),
            cleanup_started: cleanup_started.clone(),
            cleanup_release: cleanup_release.clone(),
            returned: returned.clone(),
            result: (status.clone(), payload.clone()),
        })
        .expect("register returning tool");
    let mut calls = vec![call("call_1", "partial", json!({}))];
    let peer_entered = Arc::new(Notify::new());
    let peer_returned = Arc::new(Notify::new());
    if mode == ToolExecutionMode::ParallelEligible {
        // 后一位置先返回成功，前一位置仍在清理；可靠结果仍须按调用顺序提交。
        let peer_release = CancellationToken::new();
        peer_release.cancel();
        registry
            .register(ReturnedAfterCancelTool {
                name: "peer",
                mode,
                entered: peer_entered.clone(),
                cleanup_started: Arc::new(Notify::new()),
                cleanup_release: peer_release,
                returned: peer_returned.clone(),
                result: (ToolResultStatus::Success, json!({"committed": true})),
            })
            .expect("register parallel peer");
        calls.push(call("call_2", "peer", json!({})));
    }
    let tail_id = format!("call_{}", calls.len() + 1);
    calls.push(call(&tail_id, "never", json!({})));
    registry
        .register(ScriptedTool::succeed("never", json!(null), log.clone()))
        .expect("register tail");
    let turn = calls_message("message_cancel", calls);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn))],
    ));
    let recorder = Arc::new(if record_failure {
        InMemoryRecorder::failing_at(2, log.clone())
    } else {
        InMemoryRecorder::new(log.clone())
    });
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let (input, user_input) = make_input(vec![]);
    let spec = with_guardrails(
        make_spec(
            model.clone(),
            registry.snapshot(),
            ExecutionBudget::default(),
        ),
        GuardrailConfig {
            repeated_invocation: None,
            consecutive_failures: Some(guardrail_check(ActiveGuardrailMode::Enforce, 1)),
        },
    );
    let AgentExecution {
        events,
        completion,
        control,
    } = AgentExecution::start(spec, input, make_context(recorder.clone(), authorizer));
    let collector = tokio::spawn(events.collect::<Vec<_>>());
    entered.notified().await;
    if mode == ToolExecutionMode::ParallelEligible {
        peer_entered.notified().await;
    }
    control.cancel();
    cleanup_started.notified().await;
    if mode == ToolExecutionMode::ParallelEligible {
        peer_returned.notified().await;
    }
    assert!(
        recorder.deltas().is_empty(),
        "no reliable completion before cleanup"
    );
    assert!(!log.entries().contains(&LogEntry::RecordTool));
    cleanup_release.cancel();
    let outcome = completion.await;
    returned.notified().await;
    let events = collector.await.expect("event collection");
    assert_lifecycle(&events);
    assert_eq!(model.take_requests().len(), 1, "no next model call");
    assert!(!log.entries().iter().any(|entry| matches!(
        entry,
        LogEntry::Authorize { name, .. } | LogEntry::ToolExecute { name } if name == "never"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::GuardrailTriggered { .. }))
    );
    if record_failure {
        assert_failed(
            outcome,
            ExecutionError::Record(RecordError {
                message: "injected record failure at call 2".to_owned(),
            }),
        );
        assert!(recorder.deltas().is_empty());
        assert_eq!(recorder.pending_exchanges().len(), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCompleted { .. }))
        );
        return;
    }
    assert_cancelled(outcome);
    let first_result = ToolResult {
        call_id: call_id("call_1"),
        status,
        content: ToolResultContent::json(payload),
        metadata: None,
    };
    let mut expected = vec![
        ConversationDelta::Assistant(turn),
        ConversationDelta::Tool(tool_message("toolmsg_exchange_1_1", first_result)),
    ];
    if mode == ToolExecutionMode::ParallelEligible {
        expected.push(ConversationDelta::Tool(tool_message(
            "toolmsg_exchange_1_2",
            success_result("call_2", json!({"committed": true})),
        )));
    }
    expected.push(ConversationDelta::Tool(tool_message(
        &format!("toolmsg_exchange_1_{}", expected.len()),
        error_result(&tail_id, "interrupted: execution cancelled"),
    )));
    assert_eq!(recorder.deltas(), expected);
    assert!(recorder.pending_exchanges().is_empty());
    assert_tool_pairing(&reconstruct(&user_input, &expected));
    let completed = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCompleted { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_ids = if mode == ToolExecutionMode::ParallelEligible {
        vec!["call_1", "call_2", tail_id.as_str()]
    } else {
        vec!["call_1", tail_id.as_str()]
    };
    assert_eq!(completed, expected_ids);
}

#[tokio::test]
async fn cancellation_preserves_serial_partial_result() {
    tokio::time::timeout(
        Duration::from_secs(5),
        assert_returned_cancellation_result(
            ToolExecutionMode::Serial,
            ToolResultStatus::Error,
            false,
        ),
    )
    .await
    .expect("cancellation scenario timed out");
}

#[tokio::test]
async fn cancellation_preserves_serial_success_result() {
    tokio::time::timeout(
        Duration::from_secs(5),
        assert_returned_cancellation_result(
            ToolExecutionMode::Serial,
            ToolResultStatus::Success,
            false,
        ),
    )
    .await
    .expect("cancellation scenario timed out");
}

#[tokio::test]
async fn cancellation_preserves_parallel_results_in_call_order() {
    tokio::time::timeout(
        Duration::from_secs(5),
        assert_returned_cancellation_result(
            ToolExecutionMode::ParallelEligible,
            ToolResultStatus::Error,
            false,
        ),
    )
    .await
    .expect("cancellation scenario timed out");
}

#[tokio::test]
async fn cancellation_record_failure_keeps_pending_without_completion_events() {
    tokio::time::timeout(
        Duration::from_secs(5),
        assert_returned_cancellation_result(
            ToolExecutionMode::Serial,
            ToolResultStatus::Error,
            true,
        ),
    )
    .await
    .expect("cancellation scenario timed out");
}

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
    // 取消收敛：call_1 已成功；call_2 保留清理后返回的执行错误，只有未到达的
    // call_3 补 interrupted；整批 Recorder 成功后再按原顺序发布 ToolCompleted，
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
    // 落账：Assistant + 三个按 exchange receipt 命名的 ToolMessage，配对完整。
    let deltas = recorder.deltas();
    assert_eq!(
        deltas,
        vec![
            ConversationDelta::Assistant(turn1),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_1",
                success_result("call_1", json!({"ok": true})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
                error_result("call_2", "tool execution failed: hung tool interrupted"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_3",
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
                "toolmsg_exchange_1_1",
                error_result("call_1", "interrupted: execution cancelled"),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_exchange_1_2",
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
