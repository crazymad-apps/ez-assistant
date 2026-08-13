//! 引擎行为矩阵 Harness（开发计划 §九，权威清单）。
//!
//! 全部离线、确定性：时序同步只用 gate/Notify/事件观察语义，禁止 sleep。
//! 事件生命周期由 [`finish`] 统一断言（首事件 `ExecutionStarted`、恰一个终态）。

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::Arc,
    time::Duration,
};

use agent_context::ContextWindowEvaluator;
use agent_core::{
    ActiveGuardrailMode, AgentEvent, AgentExecution, BudgetKind, CompactionReason,
    ComposedToolAuthorizer, ConversationDelta, ExecutionBudget, ExecutionConsumption,
    ExecutionContext, ExecutionError, ExecutionInput, ExecutionOutcome, ExecutionRecorder,
    ExecutionSpec, GuardrailCheckConfig, GuardrailConfig, GuardrailKind, RecordError,
    ToolAuthorization, ToolAuthorizer, ToolCompletionStatus,
};
use agent_memory::{
    MemoryPropertyValue, MemoryRecallRequest, MemoryRecallResponse, PinnedMemoryCategory,
    PinnedMemoryDraft, PinnedMemoryLimits, RecallItem, RecallOrigin, RecallSourceId,
};
use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
    ModelEventStream, ModelRequest, ModelService, ModelStreamFuture, ProviderOptions,
    ReasoningConfig, ReasoningEffort, SystemPromptSnapshot,
};
use agent_testkit::{
    AuthorizeGate, FakePinnedMemoryStore, FakeShellCompletion, FakeShellScript, FakeShellTool,
    InMemoryRecorder, LogEntry, ModelScript, OrderLog, PinnedMemoryObservation, ScriptedAuthorizer,
    ScriptedMemoryRecall, ScriptedModelService, ScriptedPolicy, ScriptedTool, ToolExecutionGate,
    message_events,
};
use agent_tools::{
    AbsolutePath, ListPinnedMemoriesTool, PinMemoryTool, RecallMemoryTool, RecallMemoryToolConfig,
    SessionPathResolver, ShellExecTool, ShellExecToolConfig, ToolExecutionMode, ToolOutputChannel,
    ToolOutputChunk, ToolRegistry, ToolSetSnapshot, UnpinMemoryTool, UpdatePinnedMemoryTool,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, FinishReason, MessageId, ModelIdentity,
    OpaqueProviderState, PartId, ProtocolId, ProviderId, ReasoningPart, TextPart, TokenUsage,
    ToolCall, ToolCallId, ToolChoice, ToolMessage, ToolName, ToolResult, ToolResultContent,
    ToolResultStatus, UserMessage, UserPart,
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

fn system_prompt() -> SystemPromptSnapshot {
    SystemPromptSnapshot::new(vec!["You are a helpful assistant.".to_owned()])
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

fn memory_limits() -> PinnedMemoryLimits {
    PinnedMemoryLimits {
        max_entries: NonZeroUsize::new(16).expect("non-zero"),
        max_id_bytes: NonZeroUsize::new(64).expect("non-zero"),
        max_category_bytes: NonZeroUsize::new(32).expect("non-zero"),
        max_content_bytes: NonZeroUsize::new(512).expect("non-zero"),
        max_attributes_per_entry: NonZeroUsize::new(8).expect("non-zero"),
        max_attribute_key_bytes: NonZeroUsize::new(32).expect("non-zero"),
        max_attribute_string_bytes: NonZeroUsize::new(128).expect("non-zero"),
        max_description_bytes: NonZeroUsize::new(512).expect("non-zero"),
        max_snapshot_bytes: NonZeroUsize::new(8192).expect("non-zero"),
    }
}

fn memory_tools_snapshot(
    store: Arc<FakePinnedMemoryStore>,
    recall: Arc<ScriptedMemoryRecall>,
) -> ToolSetSnapshot {
    let mut registry = ToolRegistry::new();
    registry
        .register(PinMemoryTool::new(store.clone(), memory_limits()))
        .expect("register pin memory");
    registry
        .register(UpdatePinnedMemoryTool::new(store.clone(), memory_limits()))
        .expect("register update pinned memory");
    registry
        .register(UnpinMemoryTool::new(store.clone(), memory_limits()))
        .expect("register unpin memory");
    registry
        .register(ListPinnedMemoriesTool::new(store))
        .expect("register list pinned memories");
    registry
        .register(RecallMemoryTool::new(
            recall,
            RecallMemoryToolConfig::new(NonZeroUsize::new(10).expect("non-zero")),
        ))
        .expect("register recall memory");
    registry.snapshot()
}

fn make_spec(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    budget: ExecutionBudget,
) -> ExecutionSpec {
    make_spec_with_threshold(model, tools, budget, 0.8)
}

fn guardrail_check(mode: ActiveGuardrailMode, threshold: u32) -> GuardrailCheckConfig {
    GuardrailCheckConfig {
        mode,
        threshold: NonZeroU32::new(threshold).expect("non-zero guardrail threshold"),
    }
}

fn with_guardrails(mut spec: ExecutionSpec, guardrails: GuardrailConfig) -> ExecutionSpec {
    spec.guardrails = Some(guardrails);
    spec
}

fn make_spec_with_threshold(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    budget: ExecutionBudget,
    threshold: f64,
) -> ExecutionSpec {
    ExecutionSpec {
        system_prompt: system_prompt(),
        model,
        context_window: Arc::new(
            ContextWindowEvaluator::new(threshold).expect("valid test threshold"),
        ),
        tools,
        model_request: agent_core::ModelRequestConfig::default(),
        budget,
        guardrails: None,
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

#[path = "engine_harness/budget.rs"]
mod budget;
#[path = "engine_harness/cancellation.rs"]
mod cancellation;
#[path = "engine_harness/context.rs"]
mod context;
#[path = "engine_harness/guardrails.rs"]
mod guardrails;
#[path = "engine_harness/model_loop.rs"]
mod model_loop;
#[path = "engine_harness/parallel.rs"]
mod parallel;
#[path = "engine_harness/recording.rs"]
mod recording;
