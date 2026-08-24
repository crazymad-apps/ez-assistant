//! v0.7.0 Core 候选 API 的本地确定性性能基线。
//!
//! 本例只测量内存中的结构处理和 scripted Agent 执行，不访问网络、用户数据文件或 Agent 工具；
//! 启动时只调用 `rustc -vV` 记录当前工具链环境。
//! 输出为一行一个 JSON 记录，便于后续版本在相同主机与 profile 下复跑比较。

use std::{
    hint::black_box,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use agent_context::{ContextLayout, ContextWindowEvaluator};
use agent_core::{
    AgentExecution, AllowAllAuthorizer, ExecutionBudget, ExecutionContext, ExecutionInput,
    ExecutionSpec, GuardrailConfig, ModelRequestConfig,
};
use agent_model::{
    ModelCallContext, ModelCapabilities, ModelEvent, ModelEventStream, ModelRequest, ModelService,
    ModelStreamFuture, SystemPromptSnapshot,
};
use agent_sdk::AgentBuilder;
use agent_testkit::{OrderLog, ScriptedTool};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolMessage,
    ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const WARMUP_ITERATIONS: u64 = 8;
const CONTEXT_TURNS: usize = 256;
const CONTEXT_ITERATIONS: u64 = 2_000;
const TOOL_COUNT: usize = 128;
const TOOL_ITERATIONS: u64 = 200;
const EVENT_DELTAS: usize = 4_096;
const EVENT_ITERATIONS: u64 = 20;
const EXECUTION_ITERATIONS: u64 = 500;
const CONCURRENT_ITERATIONS: u64 = 250;

#[derive(Serialize)]
struct BaselineRecord<'a> {
    case: &'a str,
    input_size: usize,
    iterations: u64,
    elapsed_ns: u128,
    ns_per_iteration: u128,
    target: &'a str,
    profile: &'a str,
    rustc_version: &'a str,
}

struct Environment {
    target: String,
    profile: &'static str,
    rustc_version: String,
}

impl Environment {
    fn detect() -> Self {
        let verbose = command_output("rustc", &["-vV"]);
        let target = verbose
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .unwrap_or("unknown-target")
            .to_owned();
        let rustc_version = verbose.lines().next().unwrap_or("unknown-rustc").to_owned();
        Self {
            target,
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            rustc_version,
        }
    }

    fn report(&self, case: &'static str, input_size: usize, iterations: u64, elapsed_ns: u128) {
        let record = BaselineRecord {
            case,
            input_size,
            iterations,
            elapsed_ns,
            ns_per_iteration: elapsed_ns / u128::from(iterations),
            target: &self.target,
            profile: self.profile,
            rustc_version: &self.rustc_version,
        };
        println!(
            "{}",
            serde_json::to_string(&record).expect("baseline record serializes")
        );
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default()
}

struct RepeatingModel {
    capabilities: ModelCapabilities,
    sequence: AtomicU64,
    text_deltas: usize,
}

impl RepeatingModel {
    fn new(text_deltas: usize) -> Self {
        Self {
            capabilities: ModelCapabilities {
                reasoning: false,
                image_input: false,
                tool_calls: false,
                multimodal_tool_result: false,
                tool_choice: agent_model::ToolChoiceCapabilities::default(),
                streaming: true,
            },
            sequence: AtomicU64::new(1),
            text_deltas,
        }
    }
}

impl ModelService for RepeatingModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        128_000
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let message_id = MessageId::new(format!("baseline_message_{sequence}"))
            .expect("valid baseline message id");
        let part_id =
            PartId::new(format!("baseline_part_{sequence}")).expect("valid baseline part id");
        let model = model_identity();
        let text = "x".repeat(self.text_deltas);
        let message = AssistantMessage {
            id: message_id.clone(),
            model: model.clone(),
            parts: vec![AssistantPart::Text(TextPart {
                id: part_id.clone(),
                text,
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        let mut events = Vec::with_capacity(self.text_deltas.saturating_add(4));
        events.push(ModelEvent::TurnStarted { message_id, model });
        events.push(ModelEvent::TextStarted {
            id: part_id.clone(),
        });
        events.extend((0..self.text_deltas).map(|_| ModelEvent::TextDelta {
            id: part_id.clone(),
            delta: "x".to_owned(),
        }));
        events.push(ModelEvent::TextFinished { id: part_id });
        events.push(ModelEvent::TurnFinished { message });
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as ModelEventStream) })
    }
}

fn model_identity() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("baseline").expect("valid provider id"),
        "baseline-model",
    )
}

fn execution_input() -> ExecutionInput {
    ExecutionInput {
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new("baseline_user").expect("valid user message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("baseline_user_text").expect("valid user part id"),
                text: "Complete the deterministic baseline turn.".to_owned(),
            })],
        })]),
    }
}

fn execution_context() -> ExecutionContext {
    ExecutionContext {
        cancellation: CancellationToken::new(),
        recorder: Arc::new(agent_testkit::InMemoryRecorder::new(OrderLog::new())),
        authorizer: Arc::new(AllowAllAuthorizer),
    }
}

fn execution_spec(model: Arc<dyn ModelService>) -> ExecutionSpec {
    ExecutionSpec {
        system_prompt: SystemPromptSnapshot::new(vec![
            "Run the deterministic Core baseline.".to_owned(),
        ]),
        model,
        context_window: Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
        tools: ToolSetSnapshot::default(),
        model_request: ModelRequestConfig::default(),
        budget: ExecutionBudget::default(),
        guardrails: None::<GuardrailConfig>,
    }
}

fn context_fixture(turns: usize) -> ConversationSnapshot {
    let mut messages = Vec::with_capacity(turns * 3);
    for index in 0..turns {
        messages.push(ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new(format!("context_user_{index}")).expect("valid user id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("context_user_text_{index}")).expect("valid user part id"),
                text: format!("Synthetic context turn {index}"),
            })],
        }));
        if index % 8 == 0 {
            let call_id =
                ToolCallId::new(format!("context_call_{index}")).expect("valid tool call id");
            messages.push(ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new(format!("context_assistant_{index}"))
                    .expect("valid assistant id"),
                model: model_identity(),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new("synthetic_lookup").expect("valid tool name"),
                    arguments: json!({"index": index}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }));
            messages.push(ConversationMessage::Tool(ToolMessage {
                id: MessageId::new(format!("context_tool_{index}")).expect("valid tool id"),
                result: ToolResult {
                    call_id,
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::json(json!({"index": index})),
                    metadata: None,
                },
            }));
            messages.push(ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new(format!("context_final_{index}"))
                    .expect("valid final assistant id"),
                model: model_identity(),
                parts: vec![AssistantPart::Text(TextPart {
                    id: PartId::new(format!("context_final_text_{index}"))
                        .expect("valid final assistant part id"),
                    text: format!("Synthetic tool result {index}"),
                })],
                finish_reason: FinishReason::Stop,
                usage: None,
            }));
        } else {
            messages.push(ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new(format!("context_assistant_{index}"))
                    .expect("valid assistant id"),
                model: model_identity(),
                parts: vec![AssistantPart::Text(TextPart {
                    id: PartId::new(format!("context_assistant_text_{index}"))
                        .expect("valid assistant part id"),
                    text: format!("Synthetic answer {index}"),
                })],
                finish_reason: FinishReason::Stop,
                usage: None,
            }));
        }
    }
    ConversationSnapshot::new(messages)
}

fn freeze_tools(tool_count: usize) -> ToolSetSnapshot {
    let mut registry = ToolRegistry::new();
    for index in 0..tool_count {
        registry
            .register(ScriptedTool::succeed(
                &format!("baseline_tool_{index}"),
                json!({"index": index}),
                OrderLog::new(),
            ))
            .expect("register baseline tool");
    }
    registry.snapshot()
}

fn measure_sync(mut action: impl FnMut(), iterations: u64) -> u128 {
    for _ in 0..WARMUP_ITERATIONS {
        action();
    }
    let started = Instant::now();
    for _ in 0..iterations {
        action();
    }
    started.elapsed().as_nanos()
}

async fn completed(execution: agent_core::AgentExecution, drain_events: bool) {
    let agent_core::AgentExecution {
        mut events,
        completion,
        control: _,
    } = execution;
    if drain_events {
        let event_task = tokio::spawn(async move {
            let mut count = 0_u64;
            while events.next().await.is_some() {
                count = count.saturating_add(1);
            }
            count
        });
        black_box(completion.await);
        black_box(event_task.await.expect("event consumer joins"));
    } else {
        drop(events);
        black_box(completion.await);
    }
}

async fn measure_sdk_execution(agent: &agent_sdk::Agent, iterations: u64, ephemeral: bool) -> u128 {
    for _ in 0..WARMUP_ITERATIONS {
        let execution = if ephemeral {
            agent.start_ephemeral(
                execution_input(),
                CancellationToken::new(),
                Arc::new(AllowAllAuthorizer),
            )
        } else {
            agent.start(execution_input(), execution_context())
        };
        completed(execution, false).await;
    }
    let started = Instant::now();
    for _ in 0..iterations {
        let execution = if ephemeral {
            agent.start_ephemeral(
                execution_input(),
                CancellationToken::new(),
                Arc::new(AllowAllAuthorizer),
            )
        } else {
            agent.start(execution_input(), execution_context())
        };
        completed(execution, false).await;
    }
    started.elapsed().as_nanos()
}

async fn measure_core_execution(spec: &ExecutionSpec, iterations: u64) -> u128 {
    for _ in 0..WARMUP_ITERATIONS {
        completed(
            AgentExecution::start(spec.clone(), execution_input(), execution_context()),
            false,
        )
        .await;
    }
    let started = Instant::now();
    for _ in 0..iterations {
        completed(
            AgentExecution::start(spec.clone(), execution_input(), execution_context()),
            false,
        )
        .await;
    }
    started.elapsed().as_nanos()
}

async fn measure_event_stream(drain_events: bool) -> u128 {
    let model: Arc<dyn ModelService> = Arc::new(RepeatingModel::new(EVENT_DELTAS));
    let agent = AgentBuilder::new(
        model,
        SystemPromptSnapshot::new(vec!["Emit deterministic deltas.".to_owned()]),
        Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
    )
    .build()
    .expect("valid event baseline agent");
    for _ in 0..WARMUP_ITERATIONS {
        completed(
            agent.start(execution_input(), execution_context()),
            drain_events,
        )
        .await;
    }
    let started = Instant::now();
    for _ in 0..EVENT_ITERATIONS {
        completed(
            agent.start(execution_input(), execution_context()),
            drain_events,
        )
        .await;
    }
    started.elapsed().as_nanos()
}

async fn measure_two_agents() -> u128 {
    let model: Arc<dyn ModelService> = Arc::new(RepeatingModel::new(1));
    let evaluator = Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold"));
    let first = AgentBuilder::new(
        model.clone(),
        SystemPromptSnapshot::new(vec!["First session.".to_owned()]),
        evaluator.clone(),
    )
    .build()
    .expect("valid first agent");
    let second = AgentBuilder::new(
        model,
        SystemPromptSnapshot::new(vec!["Second session.".to_owned()]),
        evaluator,
    )
    .build()
    .expect("valid second agent");
    let run_pair = || async {
        tokio::join!(
            completed(first.start(execution_input(), execution_context()), false),
            completed(second.start(execution_input(), execution_context()), false)
        );
    };
    for _ in 0..WARMUP_ITERATIONS {
        run_pair().await;
    }
    let started = Instant::now();
    for _ in 0..CONCURRENT_ITERATIONS {
        run_pair().await;
    }
    started.elapsed().as_nanos()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let environment = Environment::detect();

    let conversation = context_fixture(CONTEXT_TURNS);
    let context_elapsed = measure_sync(
        || {
            black_box(
                ContextLayout::build(black_box(&conversation)).expect("valid context fixture"),
            );
        },
        CONTEXT_ITERATIONS,
    );
    environment.report(
        "context_layout",
        conversation.messages.len(),
        CONTEXT_ITERATIONS,
        context_elapsed,
    );

    let tools_elapsed = measure_sync(
        || {
            let snapshot = freeze_tools(TOOL_COUNT);
            black_box(snapshot.definitions().len());
        },
        TOOL_ITERATIONS,
    );
    environment.report(
        "tool_set_freeze",
        TOOL_COUNT,
        TOOL_ITERATIONS,
        tools_elapsed,
    );

    environment.report(
        "event_stream_connected",
        EVENT_DELTAS,
        EVENT_ITERATIONS,
        measure_event_stream(true).await,
    );
    environment.report(
        "event_stream_disconnected",
        EVENT_DELTAS,
        EVENT_ITERATIONS,
        measure_event_stream(false).await,
    );

    environment.report(
        "two_agent_shared_model",
        2,
        CONCURRENT_ITERATIONS,
        measure_two_agents().await,
    );

    let shared_model: Arc<dyn ModelService> = Arc::new(RepeatingModel::new(1));
    let direct_spec = execution_spec(shared_model.clone());
    let sdk_agent = AgentBuilder::new(
        shared_model,
        direct_spec.system_prompt.clone(),
        direct_spec.context_window.clone(),
    )
    .build()
    .expect("valid SDK baseline agent");
    let direct_elapsed = measure_core_execution(&direct_spec, EXECUTION_ITERATIONS).await;
    environment.report(
        "core_direct_execution",
        1,
        EXECUTION_ITERATIONS,
        direct_elapsed,
    );
    let sdk_explicit_elapsed = measure_sdk_execution(&sdk_agent, EXECUTION_ITERATIONS, false).await;
    environment.report(
        "sdk_explicit_execution",
        1,
        EXECUTION_ITERATIONS,
        sdk_explicit_elapsed,
    );
    environment.report(
        "sdk_ephemeral_execution",
        1,
        EXECUTION_ITERATIONS,
        measure_sdk_execution(&sdk_agent, EXECUTION_ITERATIONS, true).await,
    );
}
