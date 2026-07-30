//! 引擎行为矩阵 Harness（开发计划 §九，权威清单）。
//!
//! 全部离线、确定性：时序同步只用 gate/Notify/事件观察语义，禁止 sleep。
//! 事件生命周期由 [`finish`] 统一断言（首事件 `ExecutionStarted`、恰一个终态）。

use std::sync::Arc;

use agent_context::ContextWindowEvaluator;
use agent_core::{
    AgentEvent, AgentExecution, BudgetKind, CompactionReason, ConversationDelta, ExecutionBudget,
    ExecutionContext, ExecutionError, ExecutionInput, ExecutionOutcome, ExecutionRecorder,
    ExecutionSpec, RecordError, ToolAuthorization, ToolAuthorizer, ToolCompletionStatus,
};
use agent_model::{
    ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelRequest,
    ModelService, ModelStreamFuture,
};
use agent_testkit::{
    AuthorizeGate, InMemoryRecorder, LogEntry, ModelScript, OrderLog, ScriptedAuthorizer,
    ScriptedModelService, ScriptedTool, message_events,
};
use agent_tools::{ToolOutputChannel, ToolOutputChunk, ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason, MessageId, ModelIdentity,
    OpaqueProviderState, PartId, ProtocolId, ProviderId, ReasoningPart, TextPart, TokenUsage,
    ToolCall, ToolCallId, ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultStatus,
    UserMessage, UserPart,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const TEST_CONTEXT_WINDOW_TOKENS: u64 = 128_000;

// ---------- 构造辅助 ----------

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        reasoning: true,
        tool_calls: true,
        streaming: true,
    }
}

fn msg_id(value: &str) -> MessageId {
    MessageId::new(value).expect("valid message id")
}

fn part_id(value: &str) -> PartId {
    PartId::new(value).expect("valid part id")
}

fn call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid call id")
}

fn tool_name(value: &str) -> ToolName {
    ToolName::new(value).expect("valid tool name")
}

fn model_identity() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("deepseek").expect("valid provider id"),
        "deepseek-reasoner",
    )
}

fn instructions() -> Vec<String> {
    vec!["You are a helpful assistant.".to_owned()]
}

fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: call_id(id),
        name: tool_name(name),
        arguments,
    }
}

/// 纯文本 Turn（finish_reason: Stop）。
fn text_message(id: &str, text: &str) -> AssistantMessage {
    AssistantMessage {
        id: msg_id(id),
        model: model_identity(),
        parts: vec![AssistantPart::Text(TextPart {
            id: part_id("text_1"),
            text: text.to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

/// 只含 Tool Call 的 Turn（finish_reason: ToolCalls）。
fn calls_message(id: &str, calls: Vec<ToolCall>) -> AssistantMessage {
    AssistantMessage {
        id: msg_id(id),
        model: model_identity(),
        parts: calls.into_iter().map(AssistantPart::ToolCall).collect(),
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn tool_message(id: &str, result: ToolResult) -> ToolMessage {
    ToolMessage {
        id: msg_id(id),
        result,
    }
}

fn success_result(id: &str, output: Value) -> ToolResult {
    ToolResult {
        call_id: call_id(id),
        status: ToolResultStatus::Success,
        content: ToolResultContent::Json(output),
    }
}

fn error_result(id: &str, message: &str) -> ToolResult {
    ToolResult {
        call_id: call_id(id),
        status: ToolResultStatus::Error,
        content: ToolResultContent::Text(message.to_owned()),
    }
}

fn snapshot_of(tools: Vec<ScriptedTool>) -> ToolSetSnapshot {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register(tool).expect("register scripted tool");
    }
    registry.snapshot()
}

fn make_spec(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    budget: ExecutionBudget,
) -> ExecutionSpec {
    make_spec_with_threshold(model, tools, budget, 0.8)
}

fn make_spec_with_threshold(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    budget: ExecutionBudget,
    threshold: f64,
) -> ExecutionSpec {
    ExecutionSpec {
        instructions: instructions(),
        model,
        context_window: Arc::new(
            ContextWindowEvaluator::new(threshold).expect("valid test threshold"),
        ),
        tools,
        budget,
    }
}

/// 默认输入：Runtime 已将固定用户消息追加到历史；返回完整输入与用户消息副本。
fn make_input(mut history: Vec<ConversationMessage>) -> (ExecutionInput, UserMessage) {
    let user_input = UserMessage {
        id: msg_id("message_u1"),
        parts: vec![UserPart::Text(TextPart {
            id: part_id("text_u1"),
            text: "What is today?".to_owned(),
        })],
    };
    history.push(ConversationMessage::User(user_input.clone()));
    (
        ExecutionInput {
            conversation: agent_types::ConversationSnapshot::new(history),
        },
        user_input,
    )
}

fn make_context(
    recorder: Arc<InMemoryRecorder>,
    authorizer: Arc<dyn ToolAuthorizer>,
) -> ExecutionContext {
    ExecutionContext {
        cancellation: CancellationToken::new(),
        recorder,
        authorizer,
    }
}

/// 收集执行结果与事件流，并断言事件生命周期（首事件 + 恰一个终态且在末尾）。
async fn finish(execution: AgentExecution) -> (ExecutionOutcome, Vec<AgentEvent>) {
    let AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    // 并发收集事件，避免 bounded 通道溢出造成丢弃计数污染断言。
    let collector = tokio::spawn(events.collect::<Vec<_>>());
    let outcome = completion.await;
    let events = collector.await.expect("event collection task panicked");
    assert_lifecycle(&events);
    (outcome, events)
}

fn assert_lifecycle(events: &[AgentEvent]) {
    assert!(
        matches!(events.first(), Some(AgentEvent::ExecutionStarted)),
        "first event must be ExecutionStarted, got {events:?}"
    );
    let terminals = events.iter().filter(|event| event.is_terminal()).count();
    assert_eq!(
        terminals, 1,
        "expected exactly one terminal event in {events:?}"
    );
    assert!(
        events.last().is_some_and(|event| event.is_terminal()),
        "last event must be the terminal one in {events:?}"
    );
}

/// 输入 + 落账增量重建"取消/终止后的新 ConversationSnapshot"。
fn reconstruct(user_input: &UserMessage, deltas: &[ConversationDelta]) -> Vec<ConversationMessage> {
    let mut messages = vec![ConversationMessage::User(user_input.clone())];
    for delta in deltas {
        messages.push(match delta {
            ConversationDelta::Assistant(message) => {
                ConversationMessage::Assistant(message.clone())
            }
            ConversationDelta::Tool(message) => ConversationMessage::Tool(message.clone()),
        });
    }
    messages
}

/// Tool Call/Result 配对断言：每个 Tool Call 恰有一个后续 Tool 结果，无孤儿。
fn assert_tool_pairing(messages: &[ConversationMessage]) {
    let mut pending: Vec<ToolCallId> = Vec::new();
    for message in messages {
        match message {
            ConversationMessage::Assistant(message) => {
                pending.extend(message.parts.iter().filter_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some(call.id.clone()),
                    _ => None,
                }));
            }
            ConversationMessage::Tool(message) => {
                let position = pending
                    .iter()
                    .position(|id| *id == message.result.call_id)
                    .unwrap_or_else(|| {
                        panic!(
                            "tool result {:?} has no matching call",
                            message.result.call_id
                        )
                    });
                pending.remove(position);
            }
            _ => {}
        }
    }
    assert!(
        pending.is_empty(),
        "tool calls without results: {pending:?}"
    );
}

// ---------- 行为矩阵 ----------

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
    assert_eq!(requests[0].system, instructions());
    assert!(requests[0].tools.is_empty());
    assert_eq!(
        requests[0].conversation.messages,
        vec![ConversationMessage::User(user_input)]
    );
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
    let authorizer = Arc::new(ScriptedAuthorizer::with_decisions(
        log.clone(),
        [(
            "write_file".to_owned(),
            ToolAuthorization::Deny {
                reason: "no writes today".to_owned(),
            },
        )],
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
    // 顺序：begin(Assistant) → authorize(read) → execute(read) → authorize(write, Deny，
    // 无 execute) → complete(batch)；write_file 从未执行。
    assert_eq!(
        log.entries(),
        vec![
            LogEntry::RecordAssistant,
            LogEntry::Authorize {
                name: "read_file".to_owned(),
                batch_size: 2,
            },
            LogEntry::ToolExecute {
                name: "read_file".to_owned(),
            },
            LogEntry::Authorize {
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
        }
    );
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::ExecutionCompactionRequired {
                reason: CompactionReason::ThresholdReached,
                step: 1,
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
            dropped_events: 0,
        })
    );
}

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
}

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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2.clone()));
    // ToolOutputChunk{channel, delta} → AgentEvent::ToolOutput{call_id, channel, chunk}。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                call: call("call_1", "chatty", json!({})),
            },
            AgentEvent::ToolStarted {
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolOutput {
                call_id: call_id("call_1"),
                channel: ToolOutputChannel::Stdout,
                chunk: "line 1".to_owned(),
            },
            AgentEvent::ToolOutput {
                call_id: call_id("call_1"),
                channel: ToolOutputChannel::Stderr,
                chunk: "warn".to_owned(),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::StepStarted { step: 2 },
            AgentEvent::TextDelta {
                id: part_id("text_1"),
                delta: "Done chatting.".to_owned(),
            },
            AgentEvent::ExecutionCompleted {
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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2.clone()));
    assert_lifecycle(&events);
    let Some(AgentEvent::ExecutionCompleted {
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

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::Record(RecordError {
            message: "injected record failure at call 1".to_owned(),
        }))
    );
    // begin(Assistant) 失败阻断后续一切副作用：无任何工具执行与授权。
    assert_eq!(log.entries(), vec![LogEntry::RecordAssistant]);
    assert!(recorder.deltas().is_empty());
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
    let (outcome, _) = finish(execution).await;

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::Record(RecordError {
            message: "injected record failure at call 2".to_owned(),
        }))
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

// ---------- 预算边界（0 / 刚好等于上限 / 同批跨越 / None 见多轮测试） ----------

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

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
            kind: BudgetKind::Steps,
            limit: 0,
        })
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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
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

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
            kind: BudgetKind::Steps,
            limit: 1,
        })
    );
    // 第一轮完整处理后，第二轮 max_steps 预检先于 Context Evaluator 受控终止
    // （无 StepStarted{2}）。
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

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 0,
        })
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
                "toolmsg_1",
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
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolCompleted {
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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
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

    assert_eq!(
        outcome,
        ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
            kind: BudgetKind::ToolCalls,
            limit: 1,
        })
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
                "toolmsg_1",
                success_result("call_1", json!({"date": "2026-07-27"})),
            )),
            ConversationDelta::Tool(tool_message(
                "toolmsg_2",
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
                call: call("call_1", "get_date", json!({})),
            },
            AgentEvent::ToolProposed {
                call: call("call_2", "get_time", json!({})),
            },
            AgentEvent::ToolStarted {
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::ToolCompleted {
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

    assert_eq!(outcome, ExecutionOutcome::Completed(turn2));
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

// ---------- 取消三处 ----------

/// 挂起流模型：放出正文 delta 后挂起，直到调用上下文取消后按契约以唯一
/// `TurnFailed(Cancelled)` 受控结束（验证"模型流中取消"的挂起桩）。
struct PausedModel {
    capabilities: ModelCapabilities,
}

impl ModelService for PausedModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        TEST_CONTEXT_WINDOW_TOKENS
    }

    fn stream(&self, _request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let cancellation = context.cancellation.clone();
        Box::pin(async move {
            let stream = futures_util::stream::unfold(0u8, move |step| {
                let cancellation = cancellation.clone();
                async move {
                    match step {
                        0 => Some((
                            ModelEvent::TurnStarted {
                                message_id: msg_id("message_1"),
                                model: model_identity(),
                            },
                            1,
                        )),
                        1 => Some((
                            ModelEvent::TextStarted {
                                id: part_id("text_1"),
                            },
                            2,
                        )),
                        2 => Some((
                            ModelEvent::TextDelta {
                                id: part_id("text_1"),
                                delta: "partial".to_owned(),
                            },
                            3,
                        )),
                        3 => {
                            // 挂起直到取消（gate 语义，非 sleep）。
                            cancellation.cancelled().await;
                            Some((
                                ModelEvent::TurnFailed {
                                    error: ModelError::Cancelled,
                                },
                                4,
                            ))
                        }
                        _ => None,
                    }
                }
            });
            Ok(Box::pin(stream) as ModelEventStream)
        })
    }
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
    assert_eq!(outcome, ExecutionOutcome::Cancelled);
    let rest: Vec<AgentEvent> = events.collect().await;
    let events = [prefix, rest].concat();
    assert_lifecycle(&events);
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::TextDelta {
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
    let execution = AgentExecution::start(
        make_spec(model, tools, ExecutionBudget::default()),
        input,
        make_context(recorder.clone(), authorizer),
    );
    let AgentExecution {
        mut events,
        completion,
        control,
    } = execution;

    // 等到第二个工具开始执行且确实进入 execute（挂起中），再取消。
    let mut prefix = Vec::new();
    loop {
        let event = events.next().await.expect("event stream open");
        let hit =
            matches!(&event, AgentEvent::ToolStarted { call_id: id } if *id == call_id("call_2"));
        prefix.push(event);
        if hit {
            break;
        }
    }
    slow_entered.notified().await;
    control.cancel();

    let outcome = completion.await;
    assert_eq!(outcome, ExecutionOutcome::Cancelled);
    // Engine 只有在 cancellation-aware tool 完成清理并返回后才能解析取消终态。
    slow_cleanup_completed.notified().await;
    let rest: Vec<AgentEvent> = events.collect().await;
    let events = [prefix, rest].concat();
    assert_lifecycle(&events);
    // 取消收敛：call_1 已成功；call_2（执行中清理完成）与 call_3（未到达）补记
    // interrupted 错误 ToolResult，各发 ToolCompleted{Failed}；唯一终态 ExecutionCancelled。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                call: call("call_1", "fast", json!({})),
            },
            AgentEvent::ToolProposed {
                call: call("call_2", "slow", json!({})),
            },
            AgentEvent::ToolProposed {
                call: call("call_3", "never", json!({})),
            },
            AgentEvent::ToolStarted {
                call_id: call_id("call_1"),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Success,
            },
            AgentEvent::ToolStarted {
                call_id: call_id("call_2"),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_2"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ToolCompleted {
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
    assert_eq!(outcome, ExecutionOutcome::Cancelled);
    assert_lifecycle(&events);
    // 两个已宣告调用均未结算：取消收敛各补 interrupted 错误 ToolResult。
    assert_eq!(
        events,
        vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolProposed {
                call: call("call_1", "read_file", json!({"path": "a.txt"})),
            },
            AgentEvent::ToolProposed {
                call: call("call_2", "write_file", json!({"path": "b.txt"})),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id("call_1"),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ToolCompleted {
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
    assert!(matches!(outcome, ExecutionOutcome::Completed(_)));
    assert_eq!(recorder.deltas().len(), 2);
}
