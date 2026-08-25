use super::*;

#[tokio::test]
async fn hallucinated_tool_with_empty_snapshot_feeds_error_result() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "ghost_tool", json!({}))]);
    let turn2 = text_message("message_2", "Imagined that one; sorry.");
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
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        // 空工具集下模型幻觉 tool call：同样转为错误 ToolResult 回喂。
        make_spec(
            model.clone(),
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, _) = finish(execution).await;

    assert_completed(outcome, turn2);
    let deltas = recorder.deltas();
    let ConversationDelta::Tool(message) = &deltas[1] else {
        panic!("second delta must be the tool result, got {deltas:?}");
    };
    assert_eq!(
        message.result,
        error_result("call_1", "unknown tool: `ghost_tool`")
    );
    // 空快照没有任何工具实现可进入。
    assert!(
        !log.entries()
            .iter()
            .any(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
    );
}

#[tokio::test]
async fn tool_output_chunks_bridge_to_agent_events() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "chatty", json!({}))]);
    let turn2 = text_message("message_2", "Done chatting.");
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
        ScriptedTool::succeed("chatty", json!({"ok": true}), log.clone()).with_output_chunks(vec![
            ToolOutputChunk {
                channel: ToolOutputChannel::Stdout,
                delta: "line 1".to_owned(),
            },
            ToolOutputChunk {
                channel: ToolOutputChannel::Stderr,
                delta: "warn".to_owned(),
            },
        ]),
    ]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_completed(outcome, turn2.clone());
    // ToolOutputChunk{channel, delta} → AgentEvent::ToolOutput{call_id, channel, chunk}。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                step: 1,
                call: call("call_1", "chatty", json!({})),
            },
            AgentEvent::ToolStarted {
                step: 1,
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolOutput {
                step: 1,
                call_id: call_id("call_1"),
                channel: ToolOutputChannel::Stdout,
                chunk: "line 1".to_owned(),
            },
            AgentEvent::ToolOutput {
                step: 1,
                call_id: call_id("call_1"),
                channel: ToolOutputChannel::Stderr,
                chunk: "warn".to_owned(),
            },
            AgentEvent::ToolCompleted {
                step: 1,
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::StepStarted { step: 2 },
            AgentEvent::TextDelta {
                step: 2,
                id: part_id("text_1"),
                delta: "Done chatting.".to_owned(),
            },
            AgentEvent::ExecutionCompleted {
                step: 2,
                message: turn2,
                dropped_events: 0,
            },
        ]
    );
}

#[tokio::test]
async fn terminal_event_survives_real_engine_event_queue_overflow() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "chatty", json!({}))]);
    let turn2 = text_message("message_2", "Done chatting.");
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
    let chunks = (0..300)
        .map(|index| ToolOutputChunk {
            channel: ToolOutputChannel::Stdout,
            delta: format!("line {index}"),
        })
        .collect();
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("chatty", json!({"ok": true}), log).with_output_chunks(chunks),
    ]);
    let (input, _) = make_input(vec![]);
    let AgentExecution {
        events,
        completion,
        control: _,
    } = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder, authorizer),
    );

    // 故意等执行结束后才消费事件，稳定填满普通事件队列。
    let outcome = completion.await;
    let events = events.collect::<Vec<_>>().await;

    assert_completed(outcome, turn2.clone());
    assert_lifecycle(&events);
    let Some(AgentEvent::ExecutionCompleted {
        step: _,
        message,
        dropped_events,
    }) = events.last()
    else {
        panic!(
            "last event must be ExecutionCompleted, got {:?}",
            events.last()
        );
    };
    assert_eq!(message, &turn2);
    assert!(
        *dropped_events > 0,
        "overflow must be reflected in the reliable terminal event"
    );
}

#[tokio::test]
async fn recorder_failure_blocks_all_side_effects() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    // 第 1 次 Recorder 调用 begin(Assistant) 即失败。
    let recorder = Arc::new(InMemoryRecorder::failing_at(1, log.clone()));
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
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::Record(RecordError {
            message: "injected record failure at call 1".to_owned(),
        }),
    );
    // begin(Assistant) 失败阻断后续一切副作用：无任何工具执行与授权。
    assert_eq!(log.entries(), vec![LogEntry::RecordAssistant]);
    assert!(recorder.deltas().is_empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCompleted { .. })),
        "ToolCompleted must not be published before recorder commit succeeds"
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::Record(RecordError {
                    message: "injected record failure at call 1".to_owned(),
                }),
                dropped_events: 0,
            },
        ]
    );
}

#[tokio::test]
async fn recorder_failure_at_started_blocks_tool_execution_and_settles_result() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "The tool could not be started.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::failing_start(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "get_date",
        json!({"date": "2026-08-11"}),
        log.clone(),
    )]);
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, _) = finish(execution).await;

    assert_completed(outcome, turn2);
    assert!(recorder.started_calls().is_empty());
    assert!(
        !log.entries()
            .iter()
            .any(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
    );
    let deltas = recorder.deltas();
    let ConversationDelta::Tool(message) = &deltas[1] else {
        panic!("second delta must be the failed tool result, got {deltas:?}");
    };
    assert_eq!(
        message.result,
        error_result("call_1", "tool execution start could not be recorded")
    );
}

#[tokio::test]
async fn recorder_failure_at_tool_record_fails_controlled() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn1))],
    ));
    // 第 2 次 Recorder 调用（complete）失败：pending 已建立、工具已执行，
    // completed 规范投影仍保持为空。
    let recorder = Arc::new(InMemoryRecorder::failing_at(2, log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "get_date",
        json!({"date": "2026-07-27"}),
        log.clone(),
    )]);
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_failed(
        outcome,
        ExecutionError::Record(RecordError {
            message: "injected record failure at call 2".to_owned(),
        }),
    );
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "get_date".to_owned(),
                batch_size: 1,
            },
            LogEntry::ToolExecute {
                name: "get_date".to_owned(),
            },
            LogEntry::RecordTool,
        ]
    );
    // complete 失败不暴露部分规范对话；pending exchange 保持可恢复。
    assert!(recorder.deltas().is_empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCompleted { .. })),
        "ToolCompleted must not be published before recorder commit succeeds"
    );
    let pending = recorder.pending_exchanges();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1, turn1);

    // 模拟 Runtime 恢复：用 interrupted 结果原子完成 pending，快照重新满足配对。
    recorder
        .complete_tool_exchange(
            &pending[0].0,
            vec![tool_message(
                "toolmsg_recovered_1",
                error_result("call_1", "interrupted: recorder recovery"),
            )],
        )
        .await
        .expect("recover pending exchange");
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn recorder_context_change_ends_the_execution_with_reliable_consumption() {
    let log = OrderLog::new();
    let turn = calls_message("message_1", vec![call("call_1", "load_context", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&turn))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()).with_continuation_required());
    let execution = AgentExecution::start(
        make_spec(
            model,
            snapshot_of(vec![ScriptedTool::succeed(
                "load_context",
                json!({"status": "staged"}),
                log.clone(),
            )]),
            ExecutionBudget::default(),
        ),
        make_input(vec![]).0,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(
        outcome,
        ExecutionOutcome::ContinuationRequired {
            reason: agent_core::ContinuationReason::ContextChanged,
            consumption: ExecutionConsumption {
                steps: 1,
                tool_calls: 1,
            },
        }
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ExecutionContinuationRequired {
            reason: agent_core::ContinuationReason::ContextChanged,
            ..
        })
    ));
    assert_eq!(recorder.deltas().len(), 2);
}

// ---------- 预算边界（0 / 刚好等于上限 / 同批跨越 / None 见多轮测试） ----------
