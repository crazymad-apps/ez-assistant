//! v0.2.0 Agent Engine 离线效果演示。
//!
//! 运行：
//!
//! ```text
//! cargo run -p agent-testkit --example engine_demo
//! ```

use std::sync::Arc;

use agent_core::{
    AgentEvent, AgentExecution, ConversationDelta, ExecutionBudget, ExecutionContext,
    ExecutionInput, ExecutionOutcome, ExecutionSpec, ToolCompletionStatus,
};
use agent_model::ModelCapabilities;
use agent_testkit::{
    InMemoryRecorder, LogEntry, ModelScript, OrderLog, ScriptedAuthorizer, ScriptedModelService,
    ScriptedTool, message_events,
};
use agent_tools::{ToolOutputChannel, ToolOutputChunk, ToolRegistry};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, ReasoningPart, TextPart, ToolCall, ToolCallId,
    ToolName, ToolResultContent, UserMessage, UserPart,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let order_log = OrderLog::new();
    let recorder = Arc::new(InMemoryRecorder::new(order_log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(order_log.clone()));

    let tool_call = ToolCall {
        id: ToolCallId::new("call_weather_1")?,
        name: ToolName::new("get_weather")?,
        arguments: json!({"city": "杭州"}),
    };
    let tool_turn = AssistantMessage {
        id: MessageId::new("assistant_tool_turn")?,
        model: demo_model(),
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: PartId::new("reasoning_1")?,
                text: "需要先查询天气工具。".to_owned(),
            }),
            AssistantPart::ToolCall(tool_call),
        ],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let final_turn = AssistantMessage {
        id: MessageId::new("assistant_final_turn")?,
        model: demo_model(),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new("answer_1")?,
            text: "杭州今天晴，26°C，适合出门。".to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        ModelCapabilities {
            reasoning: true,
            tool_calls: true,
            streaming: true,
        },
        [
            ModelScript::Events(message_events(&tool_turn)),
            ModelScript::Events(message_events(&final_turn)),
        ],
    ));

    let mut registry = ToolRegistry::new();
    registry.register(
        ScriptedTool::succeed(
            "get_weather",
            json!({"city": "杭州", "weather": "晴", "temperature_c": 26}),
            order_log.clone(),
        )
        .with_output_chunks(vec![ToolOutputChunk {
            channel: ToolOutputChannel::Stdout,
            delta: "正在读取离线天气数据……".to_owned(),
        }]),
    )?;

    let execution = AgentExecution::start(
        ExecutionSpec {
            instructions: vec!["回答前可调用已注册工具。".to_owned()],
            model: model.clone(),
            tools: registry.snapshot(),
            budget: ExecutionBudget {
                max_steps: Some(4),
                max_tool_calls: Some(2),
            },
        },
        ExecutionInput {
            conversation: ConversationSnapshot::default(),
            user_input: UserMessage {
                id: MessageId::new("user_1")?,
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("user_text_1")?,
                    text: "杭州今天天气如何？".to_owned(),
                })],
            },
        },
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: recorder.clone(),
            authorizer,
        },
    );

    println!("=== v0.2.0 Agent Engine 离线演示 ===");
    let AgentExecution {
        mut events,
        completion,
        control: _,
    } = execution;
    let event_printer = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            println!("[event] {}", describe_event(&event));
        }
    });
    let outcome = completion.await;
    event_printer.await?;

    println!("\n=== 最终结果 ===");
    match &outcome {
        ExecutionOutcome::Completed(message) => {
            println!("状态：Completed");
            println!("回答：{}", assistant_text(message));
        }
        ExecutionOutcome::Failed(error) => println!("状态：Failed ({error})"),
        ExecutionOutcome::Cancelled => println!("状态：Cancelled"),
    }

    println!("\n=== 副作用顺序 ===");
    for entry in order_log.entries() {
        println!("[order] {}", describe_order_entry(&entry));
    }

    println!("\n=== Recorder 规范投影 ===");
    for delta in recorder.deltas() {
        println!("[journal] {}", describe_delta(&delta));
    }
    println!(
        "[journal] pending exchanges: {}",
        recorder.pending_exchanges().len()
    );

    println!("\n=== 模型请求投影 ===");
    for (index, request) in model.take_requests().iter().enumerate() {
        let roles = request
            .conversation
            .messages
            .iter()
            .map(conversation_role)
            .collect::<Vec<_>>()
            .join(" → ");
        println!("[request {}] {roles}", index + 1);
    }

    Ok(())
}

fn demo_model() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("demo").expect("static provider id is valid"),
        "scripted-agent",
    )
}

fn assistant_text(message: &AssistantMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn describe_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ExecutionStarted => "execution.started".to_owned(),
        AgentEvent::StepStarted { step } => format!("step.started #{step}"),
        AgentEvent::TextDelta { delta, .. } => format!("text.delta: {delta}"),
        AgentEvent::ReasoningDelta { delta, .. } => format!("reasoning.delta: {delta}"),
        AgentEvent::ToolProposed { call } => {
            format!("tool.proposed: {} {}", call.name, call.arguments)
        }
        AgentEvent::ToolStarted { call_id } => format!("tool.started: {call_id}"),
        AgentEvent::ToolOutput { channel, chunk, .. } => {
            format!("tool.output ({channel:?}): {chunk}")
        }
        AgentEvent::ToolCompleted { call_id, status } => {
            let status = match status {
                ToolCompletionStatus::Success => "success",
                ToolCompletionStatus::Failed => "failed",
            };
            format!("tool.completed: {call_id} ({status})")
        }
        AgentEvent::ExecutionCompleted { dropped_events, .. } => {
            format!("execution.completed (dropped_events={dropped_events})")
        }
        AgentEvent::ExecutionFailed {
            error,
            dropped_events,
        } => format!("execution.failed: {error} (dropped_events={dropped_events})"),
        AgentEvent::ExecutionCancelled { dropped_events } => {
            format!("execution.cancelled (dropped_events={dropped_events})")
        }
    }
}

fn describe_order_entry(entry: &LogEntry) -> String {
    match entry {
        LogEntry::RecordAssistant => "recorder.begin(AssistantMessage)".to_owned(),
        LogEntry::RecordTool => "recorder.complete(ToolMessage batch)".to_owned(),
        LogEntry::Authorize { name, batch_size } => {
            format!("authorizer.allow({name}, batch_size={batch_size})")
        }
        LogEntry::ToolExecute { name } => format!("tool.execute({name})"),
        LogEntry::ToolCleanup { name } => format!("tool.cleanup({name})"),
    }
}

fn describe_delta(delta: &ConversationDelta) -> String {
    match delta {
        ConversationDelta::Assistant(message) => {
            let tool_calls = message
                .parts
                .iter()
                .filter(|part| matches!(part, AssistantPart::ToolCall(_)))
                .count();
            format!("AssistantMessage (tool_calls={tool_calls})")
        }
        ConversationDelta::Tool(message) => {
            let content = match &message.result.content {
                ToolResultContent::Text(text) => text.clone(),
                ToolResultContent::Json(value) => value.to_string(),
            };
            format!(
                "ToolMessage (call_id={}, status={:?}, content={content})",
                message.result.call_id, message.result.status
            )
        }
    }
}

fn conversation_role(message: &ConversationMessage) -> &'static str {
    match message {
        ConversationMessage::System(_) => "system",
        ConversationMessage::User(_) => "user",
        ConversationMessage::Assistant(_) => "assistant(tool_call)",
        ConversationMessage::Tool(_) => "tool(result)",
    }
}
