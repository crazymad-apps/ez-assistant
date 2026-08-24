use super::*;

#[tokio::test]
async fn model_establishment_failure_converges_to_failed() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::FailEstablishment(ModelError::Auth(
            "bad key".to_owned(),
        ))],
    ));
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
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::Model(ModelError::Auth(
            "bad key".to_owned()
        )))
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::Model(ModelError::Auth("bad key".to_owned())),
                dropped_events: 0,
            },
        ]
    );
    // 建立前失败：无落账、无授权、无工具执行。
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn model_in_stream_failure_converges_to_failed() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(vec![
            ModelEvent::TurnStarted {
                message_id: msg_id("message_1"),
                model: model_identity(),
            },
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
            ModelEvent::TextDelta {
                id: part_id("text_1"),
                delta: "partial".to_owned(),
            },
            ModelEvent::TurnFailed {
                error: ModelError::Provider {
                    message: "upstream exploded".to_owned(),
                    status: Some(500),
                },
            },
        ])],
    ));
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
    let (outcome, events) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::Model(ModelError::Provider {
            message: "upstream exploded".to_owned(),
            status: Some(500),
        }))
    );
    // 流中失败：已到达的 delta 照常桥接，随后受控终止。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::TextDelta {
                id: part_id("text_1"),
                delta: "partial".to_owned(),
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::Model(ModelError::Provider {
                    message: "upstream exploded".to_owned(),
                    status: Some(500),
                }),
                dropped_events: 0,
            },
        ]
    );
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn threshold_preflight_hands_off_before_step_started_or_model_call() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(capabilities(), 100, []));
    let mut previous = text_message("message_previous", "previous answer");
    previous.usage = Some(TokenUsage {
        input_tokens: 60,
        output_tokens: 20,
        total_tokens: 80,
        cached_input_tokens: None,
        reasoning_tokens: None,
    });
    let history = vec![
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: msg_id("message_previous_user"),
            parts: vec![UserPart::Text(TextPart {
                id: part_id("message_previous_user_text"),
                text: "previous question".to_owned(),
            })],
        }),
        ConversationMessage::Assistant(previous),
    ];
    let (input, _) = make_input(history);
    let execution = AgentExecution::start(
        make_spec_with_threshold(
            model.clone(),
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
            0.8,
        ),
        input,
        make_context(
            Arc::new(InMemoryRecorder::new(log.clone())),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(
        outcome,
        ExecutionOutcome::CompactionRequired {
            reason: CompactionReason::ThresholdReached,
            step: 1,
            consumption: ExecutionConsumption::default(),
        }
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::ExecutionCompactionRequired {
                reason: CompactionReason::ThresholdReached,
                step: 1,
                consumption: ExecutionConsumption::default(),
                dropped_events: 0,
            },
        ]
    );
    assert!(model.take_requests().is_empty());
}

#[tokio::test]
async fn establishment_context_overflow_converges_to_compaction_terminal() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::FailEstablishment(
            ModelError::ContextOverflow {
                message: "request exceeds context window".to_owned(),
            },
        )],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model.clone(),
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(
        outcome,
        ExecutionOutcome::CompactionRequired {
            reason: CompactionReason::ProviderOverflow,
            step: 1,
            consumption: ExecutionConsumption {
                steps: 1,
                tool_calls: 0,
            },
        }
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ExecutionCompactionRequired {
                reason: CompactionReason::ProviderOverflow,
                step: 1,
                consumption: ExecutionConsumption {
                    steps: 1,
                    tool_calls: 0,
                },
                dropped_events: 0,
            },
        ]
    );
    assert_eq!(model.take_requests().len(), 1);
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn in_stream_context_overflow_discards_partial_step_and_tool_call() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(vec![
            ModelEvent::TurnStarted {
                message_id: msg_id("message_overflow"),
                model: model_identity(),
            },
            ModelEvent::ReasoningStarted {
                id: part_id("reasoning_overflow"),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("reasoning_overflow"),
                delta: "partial reasoning".to_owned(),
            },
            ModelEvent::TextStarted {
                id: part_id("text_overflow"),
            },
            ModelEvent::TextDelta {
                id: part_id("text_overflow"),
                delta: "partial text".to_owned(),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_overflow"),
                name: tool_name("never_execute"),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_overflow"),
                arguments_delta: "{\"path\":".to_owned(),
            },
            ModelEvent::UsageUpdated {
                usage: TokenUsage {
                    input_tokens: 80,
                    output_tokens: 10,
                    total_tokens: 90,
                    cached_input_tokens: None,
                    reasoning_tokens: Some(5),
                },
            },
            ModelEvent::TurnFailed {
                error: ModelError::ContextOverflow {
                    message: "stream exceeded context window".to_owned(),
                },
            },
        ])],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model,
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(
        outcome,
        ExecutionOutcome::CompactionRequired {
            reason: CompactionReason::ProviderOverflow,
            step: 1,
            consumption: ExecutionConsumption {
                steps: 1,
                tool_calls: 0,
            },
        }
    );
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        id: part_id("reasoning_overflow"),
        delta: "partial reasoning".to_owned(),
    }));
    assert!(events.contains(&AgentEvent::TextDelta {
        id: part_id("text_overflow"),
        delta: "partial text".to_owned(),
    }));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolProposed { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::UsageUpdated { .. }))
    );
    assert_eq!(
        events.last(),
        Some(&AgentEvent::ExecutionCompactionRequired {
            reason: CompactionReason::ProviderOverflow,
            step: 1,
            consumption: ExecutionConsumption {
                steps: 1,
                tool_calls: 0,
            },
            dropped_events: 0,
        })
    );
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
}

#[tokio::test]
async fn overflow_after_completed_tool_exchange_does_not_replay_side_effects() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_tool", vec![call("call_1", "get_date", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::FailEstablishment(ModelError::ContextOverflow {
                message: "second step exceeds context window".to_owned(),
            }),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "get_date",
        json!({"date": "2026-07-29"}),
        log.clone(),
    )]);
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(
            recorder.clone(),
            Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(
        outcome,
        ExecutionOutcome::CompactionRequired {
            reason: CompactionReason::ProviderOverflow,
            step: 2,
            consumption: ExecutionConsumption {
                steps: 2,
                tool_calls: 1,
            },
        }
    );
    assert_eq!(
        log.entries()
            .iter()
            .filter(|entry| matches!(entry, LogEntry::ToolExecute { .. }))
            .count(),
        1
    );
    assert_eq!(recorder.deltas().len(), 2);
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
    assert!(events.contains(&AgentEvent::StepStarted { step: 2 }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::ExecutionCompactionRequired {
            reason: CompactionReason::ProviderOverflow,
            step: 2,
            consumption: ExecutionConsumption {
                steps: 2,
                tool_calls: 1,
            },
            dropped_events: 0,
        })
    );
}

#[tokio::test]
async fn compaction_consumption_does_not_depend_on_droppable_observation_events() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_tool", vec![call("call_1", "chatty", json!({}))]);
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::FailEstablishment(ModelError::ContextOverflow {
                message: "second step exceeds context window".to_owned(),
            }),
        ],
    ));
    let chunks = (0..300)
        .map(|index| ToolOutputChunk {
            channel: ToolOutputChannel::Stdout,
            delta: format!("line {index}"),
        })
        .collect();
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("chatty", json!({"ok": true}), log.clone())
            .with_output_chunks(chunks),
    ]);
    let (input, _) = make_input(vec![]);
    let AgentExecution {
        events,
        completion,
        control: _,
    } = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(
            Arc::new(InMemoryRecorder::new(log)),
            Arc::new(ScriptedAuthorizer::allow_all(OrderLog::new())),
        ),
    );

    // 故意等 Core 结束后才排空事件，确保普通事件通道已经发生背压丢弃。
    let outcome = completion.await;
    let events = events.collect::<Vec<_>>().await;
    let expected = ExecutionConsumption {
        steps: 2,
        tool_calls: 1,
    };
    assert!(matches!(
        outcome,
        ExecutionOutcome::CompactionRequired {
            consumption,
            ..
        } if consumption == expected
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ExecutionCompactionRequired {
            consumption,
            dropped_events,
            ..
        }) if *consumption == expected && *dropped_events > 0
    ));
}
