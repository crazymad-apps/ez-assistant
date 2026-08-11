use super::*;

#[tokio::test]
async fn plain_text_completes_with_empty_tool_set() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(message_events(&text_message(
            "message_1",
            "Hi there.",
        )))],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model.clone(),
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    let expected = text_message("message_1", "Hi there.");
    assert_eq!(outcome, ExecutionOutcome::Completed(expected.clone()));
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::TextDelta {
                id: part_id("text_1"),
                delta: "Hi there.".to_owned(),
            },
            AgentEvent::ExecutionCompleted {
                message: expected,
                dropped_events: 0,
            },
        ]
    );
    // 纯文本路径：无落账（最终消息 Core 不落账）、无授权、无工具执行；模型只调用一次。
    assert!(recorder.deltas().is_empty());
    assert!(log.entries().is_empty());
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system, system_prompt());
    assert!(requests[0].tools.is_empty());
    assert_eq!(requests[0].tool_choice, ToolChoice::Auto);
    assert_eq!(requests[0].generation, GenerationConfig::default());
    assert_eq!(requests[0].reasoning, None);
    assert!(requests[0].provider_options.is_empty());
    assert_eq!(
        requests[0].conversation.messages,
        vec![ConversationMessage::User(user_input)]
    );
}

#[tokio::test]
async fn model_request_config_is_reused_across_every_tool_loop_step() {
    let log = OrderLog::new();
    let tool_name = tool_name("configured_tool");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&calls_message(
                "message_config_tool",
                vec![call("call_config", tool_name.as_str(), json!({"value": 1}))],
            ))),
            ModelScript::Events(message_events(&text_message(
                "message_config_final",
                "configured",
            ))),
        ],
    ));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        tool_name.as_str(),
        json!({"ok": true}),
        log.clone(),
    )]);
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert("fixture", json!({"mode": "strict"}))
        .expect("valid fixture options");
    let expected_config = agent_core::ModelRequestConfig {
        tool_choice: ToolChoice::Named(tool_name),
        generation: GenerationConfig {
            temperature: Some(0.25),
            top_p: Some(0.8),
            max_output_tokens: Some(512),
            stop: vec!["done".to_owned()],
        },
        reasoning: Some(ReasoningConfig {
            effort: Some(ReasoningEffort::High),
        }),
        provider_options,
    };
    let mut spec = make_spec(model.clone(), tools, ExecutionBudget::default());
    spec.model_request = expected_config.clone();
    let (input, _) = make_input(vec![]);

    let (outcome, _) = finish(AgentExecution::start(
        spec,
        input,
        make_context(recorder, authorizer),
    ))
    .await;
    assert!(matches!(outcome, ExecutionOutcome::Completed(_)));

    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request.tool_choice, expected_config.tool_choice);
        assert_eq!(request.generation, expected_config.generation);
        assert_eq!(request.reasoning, expected_config.reasoning);
        assert_eq!(request.provider_options, expected_config.provider_options);
    }
}

#[tokio::test]
async fn successful_step_emits_only_one_final_usage_update() {
    let log = OrderLog::new();
    let provisional_usage = TokenUsage {
        input_tokens: 40,
        output_tokens: 5,
        total_tokens: 45,
        cached_input_tokens: Some(16),
        reasoning_tokens: Some(2),
    };
    let final_usage = TokenUsage {
        input_tokens: 40,
        output_tokens: 10,
        total_tokens: 50,
        cached_input_tokens: Some(16),
        reasoning_tokens: Some(4),
    };
    let mut message = text_message("message_usage", "Done.");
    message.usage = Some(final_usage.clone());
    let mut model_events = message_events(&message);
    let final_usage_index = model_events
        .iter()
        .position(|event| matches!(event, ModelEvent::UsageUpdated { .. }))
        .expect("message events include final usage");
    model_events.insert(
        final_usage_index,
        ModelEvent::UsageUpdated {
            usage: provisional_usage,
        },
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::Events(model_events)],
    ));
    let (input, _) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(
            model,
            ToolSetSnapshot::default(),
            ExecutionBudget::default(),
        ),
        input,
        make_context(
            Arc::new(InMemoryRecorder::new(log.clone())),
            Arc::new(ScriptedAuthorizer::allow_all(log)),
        ),
    );

    let (outcome, events) = finish(execution).await;
    assert_eq!(outcome, ExecutionOutcome::Completed(message));
    let usage_events = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::UsageUpdated { step, usage } => Some((*step, usage.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(usage_events, vec![(1, final_usage)]);
}

#[tokio::test]
async fn single_tool_round_trip_in_strict_side_effect_order() {
    let log = OrderLog::new();
    let turn1 = calls_message("message_1", vec![call("call_1", "get_date", json!({}))]);
    let turn2 = text_message("message_2", "Today is 2026-07-27.");
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
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2.clone()));
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolStarted {
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::StepStarted { step: 2 },
            AgentEvent::TextDelta {
                id: part_id("text_1"),
                delta: "Today is 2026-07-27.".to_owned(),
            },
            AgentEvent::ExecutionCompleted {
                message: turn2.clone(),
                dropped_events: 0,
            },
        ]
    );
    // 副作用前顺序：begin(Assistant) → authorize → execute → complete(batch)。
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
    let result = success_result("call_1", json!({"date": "2026-07-27"}));
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1.clone()),
            ConversationDelta::Tool(tool_message("toolmsg_1", result.clone())),
        ]
    );
    // ToolResult 回填进下一轮请求的 conversation（Assistant 在前、Tool 在后）。
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.messages,
        vec![
            ConversationMessage::User(user_input.clone()),
            ConversationMessage::Assistant(turn1),
            ConversationMessage::Tool(tool_message("toolmsg_1", result)),
        ]
    );
    // 配对完整。
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn same_batch_allow_and_deny_mix_settles_and_continues() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_1",
        vec![
            call("call_1", "read_file", json!({"path": "a.txt"})),
            call("call_2", "write_file", json!({"path": "b.txt"})),
        ],
    );
    let turn2 = text_message("message_2", "Read it; the write was denied.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let fallback = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let authorizer = Arc::new(ComposedToolAuthorizer::new(
        vec![Arc::new(ScriptedPolicy::with_decisions(
            log.clone(),
            [(
                "write_file".to_owned(),
                ToolAuthorization::Deny {
                    reason: "no writes today".to_owned(),
                },
            )],
        ))],
        fallback,
    ));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("read_file", json!({"content": "hello"}), log.clone()),
        ScriptedTool::succeed("write_file", json!({"written": true}), log.clone()),
    ]);
    let (input, user_input) = make_input(vec![]);
    let execution = AgentExecution::start(
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    // 顺序：begin(Assistant) → policy(read, Continue) → fallback authorize
    // → execute(read) → policy(write, Deny，不进 fallback/不 execute) → complete。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::PolicyEvaluate {
                name: "read_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::Authorize {
                name: "read_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::ToolExecute {
                name: "read_file".to_owned(),
            },
            LogEntry::PolicyEvaluate {
                name: "write_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::RecordTool,
        ]
    );
    // Deny 在授权闸处转换为错误 ToolResult（reason 是模型唯一可见信息）。
    let allow_result = success_result("call_1", json!({"content": "hello"}));
    let deny_result = error_result("call_2", "no writes today");
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(turn1.clone()),
            ConversationDelta::Tool(tool_message("toolmsg_1", allow_result.clone())),
            ConversationDelta::Tool(tool_message("toolmsg_2", deny_result.clone())),
        ]
    );
    // 事件：被拒绝的调用无 ToolStarted，只有 ToolCompleted{Failed}。
    assert!(!events.contains(&AgentEvent::ToolStarted {
        call_id: call_id("call_2"),
    }));
    assert!(events.contains(&AgentEvent::ToolCompleted {
        call_id: call_id("call_2"),
        status: ToolCompletionStatus::Failed,
    }));
    // 循环继续：第二轮请求的 conversation 含两个回填的 ToolResult（含 Deny 转换结果）。
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.messages,
        vec![
            ConversationMessage::User(user_input.clone()),
            ConversationMessage::Assistant(turn1),
            ConversationMessage::Tool(tool_message("toolmsg_1", allow_result)),
            ConversationMessage::Tool(tool_message("toolmsg_2", deny_result)),
        ]
    );
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn later_execution_skips_tool_message_ids_already_used_by_the_conversation() {
    let log = OrderLog::new();
    let prior_user = UserMessage {
        id: msg_id("message_u0"),
        parts: vec![UserPart::Text(TextPart {
            id: part_id("text_u0"),
            text: "Read the first value.".to_owned(),
        })],
    };
    let prior_assistant = calls_message(
        "message_previous_tool",
        vec![call("call_previous", "read_value", json!({}))],
    );
    let prior_result = success_result("call_previous", json!({"value": 1}));
    let history = vec![
        ConversationMessage::User(prior_user),
        ConversationMessage::Assistant(prior_assistant),
        ConversationMessage::Tool(tool_message("toolmsg_1", prior_result)),
    ];
    let current_assistant = calls_message(
        "message_current_tool",
        vec![call("call_current", "read_value", json!({}))],
    );
    let final_assistant = text_message("message_current_final", "Read both values.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&current_assistant)),
            ModelScript::Events(message_events(&final_assistant)),
        ],
    ));
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![ScriptedTool::succeed(
        "read_value",
        json!({"value": 2}),
        log,
    )]);
    let (input, _) = make_input(history);

    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, _) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(final_assistant));
    assert_eq!(
        recorder.deltas(),
        vec![
            ConversationDelta::Assistant(current_assistant),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
                success_result("call_current", json!({"value": 2})),
            )),
        ]
    );
}

#[tokio::test]
async fn multi_turn_loop_backfills_projection_with_part_fidelity() {
    let log = OrderLog::new();
    let provider_state = OpaqueProviderState::new(
        ProviderId::new("deepseek").expect("valid provider id"),
        ProtocolId::new("chat").expect("valid protocol id"),
        "reasoning_blob",
        "application/octet-stream",
        1,
        vec![0xDE, 0xAD],
    )
    .expect("valid provider state");
    let turn1 = AssistantMessage {
        id: msg_id("message_1"),
        model: model_identity(),
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("reasoning_1"),
                text: "Need the date first".to_owned(),
            }),
            AssistantPart::ToolCall(call("call_1", "get_date", json!({}))),
            AssistantPart::ProviderState(provider_state),
        ],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let turn2 = calls_message("message_2", vec![call("call_2", "get_time", json!({}))]);
    let turn3 = text_message("message_3", "It is 2026-07-27 10:00.");
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
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log.clone()));
    let tools = snapshot_of(vec![
        ScriptedTool::succeed("get_date", json!({"date": "2026-07-27"}), log.clone()),
        ScriptedTool::succeed("get_time", json!({"time": "10:00"}), log.clone()),
    ]);
    let definitions = tools.definitions().to_vec();
    // 输入快照含历史：投影起点 = 历史 + 本轮用户输入。
    let history = vec![ConversationMessage::User(UserMessage {
        id: msg_id("message_0"),
        parts: vec![UserPart::Text(TextPart {
            id: part_id("text_0"),
            text: "Earlier question".to_owned(),
        })],
    })];
    let (input, user_input) = make_input(history.clone());
    let execution = AgentExecution::start(
        // 默认预算（None/None）不限制：三轮工具循环照常完成。
        make_spec(model.clone(), tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let (outcome, events) = finish(execution).await;

    assert_eq!(outcome, ExecutionOutcome::Completed(turn3));
    // reasoning delta 桥接为同名 AgentEvent。
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        id: part_id("reasoning_1"),
        delta: "Need the date first".to_owned(),
    }));
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    let expected_system = system_prompt();
    assert!(
        requests
            .iter()
            .all(|request| request.system == expected_system),
        "every model step must reuse the frozen system prompt"
    );
    // 第一轮：历史 + 用户输入；工具定义随快照原样下发。
    assert_eq!(requests[0].tools, definitions);
    let mut expected = history;
    expected.push(ConversationMessage::User(user_input.clone()));
    assert_eq!(requests[0].conversation.messages, expected);
    // 第二轮：turn1 完整回填（reasoning / ProviderState 保真）+ toolmsg_1。
    expected.push(ConversationMessage::Assistant(turn1.clone()));
    expected.push(ConversationMessage::Tool(tool_message(
        "toolmsg_1",
        success_result("call_1", json!({"date": "2026-07-27"})),
    )));
    assert_eq!(requests[1].conversation.messages, expected);
    // 第三轮：turn2 + toolmsg_2。
    expected.push(ConversationMessage::Assistant(turn2.clone()));
    expected.push(ConversationMessage::Tool(tool_message(
        "toolmsg_2",
        success_result("call_2", json!({"time": "10:00"})),
    )));
    assert_eq!(requests[2].conversation.messages, expected);
    // 多轮后 Tool Call/Result 配对完整。
    assert_tool_pairing(&reconstruct(&user_input, &recorder.deltas()));
}

#[tokio::test]
async fn memory_tools_use_the_ordinary_loop_and_keep_the_system_prompt_frozen() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_memory_1",
        vec![call(
            "call_memory_1",
            "pin_memory",
            json!({
                "category": "preference",
                "content": "Use dark mode",
                "attributes": {"scope": "desktop"}
            }),
        )],
    );
    let turn2 = calls_message(
        "message_memory_2",
        vec![call(
            "call_memory_2",
            "recall_memory",
            json!({"query": "preferred editor", "limit": 2, "sources": ["notes"]}),
        )],
    );
    let turn3 = text_message(
        "message_memory_3",
        "I saved the preference and recalled the editor note.",
    );
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
            ModelScript::Events(message_events(&turn3)),
        ],
    ));
    let store = Arc::new(FakePinnedMemoryStore::new(vec![]));
    let recall_response = MemoryRecallResponse {
        items: vec![RecallItem {
            content: "The preferred editor is Helix".to_owned(),
            origins: vec![RecallOrigin {
                source_id: RecallSourceId::new("notes").expect("valid source id"),
                reference: Some("note-editor".to_owned()),
            }],
            attributes: BTreeMap::new(),
        }],
        failures: vec![],
        truncated: false,
    };
    let recall = Arc::new(ScriptedMemoryRecall::new(Ok(recall_response.clone())));
    let tools = memory_tools_snapshot(store.clone(), recall.clone());
    let frozen_prompt = SystemPromptSnapshot::new(vec![
        "You are a helpful assistant.".to_owned(),
        "<pinned_memories><entries></entries></pinned_memories>".to_owned(),
    ]);
    let mut spec = make_spec(model.clone(), tools, ExecutionBudget::default());
    spec.system_prompt = frozen_prompt.clone();
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let (input, user_input) = make_input(vec![]);

    let (outcome, _) = finish(AgentExecution::start(
        spec,
        input,
        make_context(recorder.clone(), authorizer.clone()),
    ))
    .await;
    assert_eq!(outcome, ExecutionOutcome::Completed(turn3));

    assert_eq!(store.entries().len(), 1);
    assert_eq!(store.entries()[0].content, "Use dark mode");
    assert_eq!(
        store.observations(),
        vec![PinnedMemoryObservation::Pin(PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "Use dark mode".to_owned(),
            attributes: BTreeMap::from([(
                "scope".to_owned(),
                MemoryPropertyValue::String("desktop".to_owned()),
            )]),
        })]
    );
    assert_eq!(
        recall.requests(),
        vec![MemoryRecallRequest {
            query: "preferred editor".to_owned(),
            limit: NonZeroUsize::new(2).expect("non-zero"),
            sources: Some(vec![RecallSourceId::new("notes").expect("valid source id")]),
        }]
    );
    assert_eq!(
        authorizer
            .observations()
            .iter()
            .map(|observation| observation.tool_name.as_str())
            .collect::<Vec<_>>(),
        vec!["pin_memory", "recall_memory"]
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.system == frozen_prompt),
        "Store changes must not refresh this execution's system prompt"
    );
    assert!(requests.iter().all(|request| request.tools.len() == 5));

    let deltas = recorder.deltas();
    let tool_results = deltas
        .iter()
        .filter_map(|delta| match delta {
            ConversationDelta::Tool(message) => Some(&message.result),
            ConversationDelta::Assistant(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 2);
    assert!(
        tool_results
            .iter()
            .all(|result| result.status == ToolResultStatus::Success)
    );
    assert_eq!(
        tool_results[1].content,
        ToolResultContent::Json(
            serde_json::to_value(recall_response).expect("serialize recall response")
        )
    );
    assert_tool_pairing(&reconstruct(&user_input, &deltas));
}

#[tokio::test]
async fn denied_memory_tools_do_not_touch_store_or_recall_capabilities() {
    let log = OrderLog::new();
    let turn1 = calls_message(
        "message_memory_deny_1",
        vec![
            call(
                "call_memory_deny_1",
                "pin_memory",
                json!({
                    "category": "preference",
                    "content": "Use dark mode",
                    "attributes": {}
                }),
            ),
            call(
                "call_memory_deny_2",
                "recall_memory",
                json!({"query": "private note", "limit": 1}),
            ),
        ],
    );
    let turn2 = text_message("message_memory_deny_2", "Both memory actions were denied.");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&turn1)),
            ModelScript::Events(message_events(&turn2)),
        ],
    ));
    let store = Arc::new(FakePinnedMemoryStore::new(vec![]));
    let recall = Arc::new(ScriptedMemoryRecall::new(Ok(MemoryRecallResponse {
        items: vec![],
        failures: vec![],
        truncated: false,
    })));
    let tools = memory_tools_snapshot(store.clone(), recall.clone());
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::with_decisions(
        log,
        [
            (
                "pin_memory".to_owned(),
                ToolAuthorization::Deny {
                    reason: "pinned writes are disabled".to_owned(),
                },
            ),
            (
                "recall_memory".to_owned(),
                ToolAuthorization::Deny {
                    reason: "recall is disabled".to_owned(),
                },
            ),
        ],
    ));
    let (input, user_input) = make_input(vec![]);

    let (outcome, events) = finish(AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer.clone()),
    ))
    .await;
    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
    assert!(store.entries().is_empty());
    assert!(store.observations().is_empty());
    assert!(recall.requests().is_empty());
    assert_eq!(authorizer.observations().len(), 2);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolStarted { .. }))
    );

    let deltas = recorder.deltas();
    let results = deltas
        .iter()
        .filter_map(|delta| match delta {
            ConversationDelta::Tool(message) => Some(&message.result),
            ConversationDelta::Assistant(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|result| result.status == ToolResultStatus::Error)
    );
    assert_tool_pairing(&reconstruct(&user_input, &deltas));
}
