use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use agent_context::ContextWindowEvaluator;
use agent_core::{ExecutionInput, ExecutionOutcome};
use agent_model::{
    ModelCallContext, ModelCapabilities, ModelError, ModelEventStream, ModelRequest, ModelService,
    ModelStreamFuture, SystemPromptSnapshot,
};
use agent_sdk::{AgentBuilder, AllowAllAuthorizer};
use agent_testkit::message_events;
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, UserMessage, UserPart,
};
use futures_util::stream;
use tokio::sync::Barrier;
use tokio_util::sync::CancellationToken;

const CONTEXT_WINDOW_TOKENS: u64 = 16_384;

struct SharedGateModel {
    capabilities: ModelCapabilities,
    entered: Arc<Barrier>,
    release: CancellationToken,
    requests: AtomicUsize,
    message: AssistantMessage,
}

impl ModelService for SharedGateModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        CONTEXT_WINDOW_TOKENS
    }

    fn stream(&self, _request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.clone();
        let release = self.release.clone();
        let events = message_events(&self.message);
        Box::pin(async move {
            entered.wait().await;
            tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => Err(ModelError::Cancelled),
                _ = release.cancelled() => {
                    Ok(Box::pin(stream::iter(events)) as ModelEventStream)
                }
            }
        })
    }
}

fn input(id: &str) -> ExecutionInput {
    ExecutionInput {
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: MessageId::new(id).expect("valid message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("{id}_text")).expect("valid part id"),
                text: "hello".to_owned(),
            })],
        })]),
    }
}

fn answer() -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("answer").expect("valid message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("valid provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new("answer_text").expect("valid part id"),
            text: "done".to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

#[tokio::test]
async fn agents_share_model_service_while_cancellation_remains_execution_local() {
    let entered = Arc::new(Barrier::new(3));
    let release = CancellationToken::new();
    let model = Arc::new(SharedGateModel {
        capabilities: ModelCapabilities {
            reasoning: false,
            image_input: false,
            tool_calls: false,
            multimodal_tool_result: false,
            tool_choice: agent_model::ToolChoiceCapabilities::default(),
            streaming: true,
        },
        entered: entered.clone(),
        release: release.clone(),
        requests: AtomicUsize::new(0),
        message: answer(),
    });
    let evaluator = Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold"));
    let first = AgentBuilder::new(
        model.clone(),
        SystemPromptSnapshot::new(vec!["first session".to_owned()]),
        evaluator.clone(),
    )
    .build()
    .expect("first agent builds");
    let second = AgentBuilder::new(
        model.clone(),
        SystemPromptSnapshot::new(vec!["second session".to_owned()]),
        evaluator,
    )
    .build()
    .expect("second agent builds");

    let first_execution = first.start_ephemeral(
        input("first_user"),
        CancellationToken::new(),
        Arc::new(AllowAllAuthorizer),
    );
    let second_execution = second.start_ephemeral(
        input("second_user"),
        CancellationToken::new(),
        Arc::new(AllowAllAuthorizer),
    );
    entered.wait().await;
    first_execution.control.cancel();
    release.cancel();

    assert_eq!(
        first_execution.completion.await,
        ExecutionOutcome::Cancelled
    );
    assert!(matches!(
        second_execution.completion.await,
        ExecutionOutcome::Completed(message) if message == answer()
    ));
    assert_eq!(model.requests.load(Ordering::SeqCst), 2);
}
