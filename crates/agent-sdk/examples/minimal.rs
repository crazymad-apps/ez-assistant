use std::sync::Arc;

use agent_sdk::{
    AgentBuilder, AllowAllAuthorizer, ContextWindowEvaluator, ExecutionInput, ExecutionOutcome,
    SystemPromptSnapshot,
};
use agent_testkit::ScriptedModelService;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, UserMessage, UserPart,
};
use tokio_util::sync::CancellationToken;

fn assistant_message() -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("answer").expect("valid fixture message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("valid fixture provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new("answer_text").expect("valid fixture part id"),
            text: "Hello from the SDK.".to_owned(),
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
                text: "Say hello.".to_owned(),
            })],
        })]),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let model = Arc::new(ScriptedModelService::completing(
        agent_model::ModelCapabilities {
            reasoning: false,
            image_input: false,
            tool_calls: false,
            streaming: true,
        },
        16_384,
        assistant_message(),
    ));
    let agent = AgentBuilder::new(
        model,
        SystemPromptSnapshot::new(vec!["You are a minimal offline agent.".to_owned()]),
        Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
    )
    .build()
    .expect("valid offline agent");

    let outcome = agent
        .start_ephemeral(
            input(),
            CancellationToken::new(),
            Arc::new(AllowAllAuthorizer),
        )
        .completion
        .await;

    match outcome {
        ExecutionOutcome::Completed(message) => println!("completed: {message:?}"),
        other => println!("unexpected outcome: {other:?}"),
    }
}
