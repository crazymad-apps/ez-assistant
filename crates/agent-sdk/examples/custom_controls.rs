use std::sync::Arc;

use agent_sdk::{
    AgentBuilder, ContextWindowEvaluator, ExecutionContext, ExecutionInput, ExecutionOutcome,
    SystemPromptSnapshot,
};
use agent_testkit::{
    InMemoryRecorder, ModelScript, OrderLog, ScriptedAuthorizer, ScriptedModelService,
    ScriptedTool, message_events,
};
use agent_tools::ToolRegistry;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolName,
    UserMessage, UserPart,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn identity() -> ModelIdentity {
    ModelIdentity::new(
        ProviderId::new("fixture").expect("valid fixture provider id"),
        "fixture-model",
    )
}

fn tool_turn() -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("tool_turn").expect("valid fixture message id"),
        model: identity(),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("call_1").expect("valid fixture call id"),
            name: ToolName::new("lookup").expect("valid fixture tool name"),
            arguments: json!({"topic": "sdk"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn final_turn() -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("answer").expect("valid fixture message id"),
        model: identity(),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new("answer_text").expect("valid fixture part id"),
            text: "The explicit controls path completed.".to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

fn input() -> ExecutionInput {
    ExecutionInput {
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: MessageId::new("user_1").expect("valid fixture message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("user_text").expect("valid fixture part id"),
                text: "Use the lookup tool.".to_owned(),
            })],
        })]),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let log = OrderLog::new();
    let model = Arc::new(ScriptedModelService::new(
        agent_model::ModelCapabilities {
            reasoning: false,
            image_input: false,
            tool_calls: true,
            multimodal_tool_result: false,
            tool_choice: agent_model::ToolChoiceCapabilities::all(),
            streaming: true,
        },
        16_384,
        [
            ModelScript::Events(message_events(&tool_turn())),
            ModelScript::Events(message_events(&final_turn())),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(ScriptedTool::succeed(
            "lookup",
            json!({"result": "offline fixture"}),
            log.clone(),
        ))
        .expect("register fixture tool");
    let recorder = Arc::new(InMemoryRecorder::new(log.clone()));
    let authorizer = Arc::new(ScriptedAuthorizer::allow_all(log));
    let agent = AgentBuilder::new(
        model,
        SystemPromptSnapshot::new(vec!["Use the registered tool once.".to_owned()]),
        Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
    )
    .tools(registry.snapshot())
    .build()
    .expect("valid offline agent");

    let execution = agent.start(
        input(),
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: recorder.clone(),
            authorizer,
        },
    );
    let agent_sdk::AgentExecution {
        mut events,
        completion,
        control,
    } = execution;
    let event_task = tokio::spawn(async move {
        let mut count = 0_u64;
        while events.next().await.is_some() {
            count += 1;
        }
        count
    });

    // `control.cancel()` 可由宿主在需要时调用；本示例保留句柄并让执行正常结束。
    let _control = control;
    let outcome = completion.await;
    let event_count = event_task.await.expect("event task joins");
    match outcome {
        ExecutionOutcome::Completed(message) => println!(
            "completed: {message:?}; events={event_count}; recorded_deltas={}",
            recorder.deltas().len()
        ),
        other => println!("unexpected outcome: {other:?}"),
    }
}
