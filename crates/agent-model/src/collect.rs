//! 单次模型 Turn 的统一收集入口。

use agent_types::AssistantMessage;
use futures_util::StreamExt;

use crate::{
    LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest, ModelService,
};

/// 执行并收集一次已经完整装配的 Provider-neutral 模型 Turn。
///
/// 本函数只负责调用现成的 [`ModelService`]、强制校验事件生命周期并返回最终
/// [`AssistantMessage`]。模型选择、请求构造、超时、Session 状态和领域结果解释仍由调用方负责。
///
/// # Errors
///
/// 建流前错误原样返回；建流后的失败、取消、缺少终态或其他生命周期错误均通过
/// [`ModelError`] 收敛。
pub async fn collect_model_turn(
    model: &dyn ModelService,
    request: ModelRequest,
    context: ModelCallContext,
) -> Result<AssistantMessage, ModelError> {
    let stream = model.stream(request, context).await?;
    let mut stream = LifecycleValidator::new(stream);
    while let Some(event) = stream.next().await {
        match event {
            ModelEvent::TurnFinished { message } => return Ok(message),
            ModelEvent::TurnFailed { error } => return Err(error),
            _ => {}
        }
    }
    Err(ModelError::Protocol(
        "validated model stream ended without a terminal event".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, TextPart, TokenUsage,
    };
    use futures_util::stream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{ModelCapabilities, ModelEventStream, ModelStreamFuture};

    struct ScriptedModel {
        stream_error: Option<ModelError>,
        events: Vec<ModelEvent>,
        capabilities: ModelCapabilities,
    }

    impl ModelService for ScriptedModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            8_192
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(async move {
                if let Some(error) = self.stream_error.clone() {
                    return Err(error);
                }
                Ok(Box::pin(stream::iter(self.events.clone())) as ModelEventStream)
            })
        }
    }

    fn message() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant-1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("valid provider id"),
                "fixture-model",
            ),
            parts: vec![agent_types::AssistantPart::Text(TextPart {
                id: agent_types::PartId::new("text-1").expect("valid part id"),
                text: "done".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: Some(TokenUsage {
                input_tokens: 4,
                output_tokens: 2,
                total_tokens: 6,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            system: crate::SystemPromptSnapshot::new(Vec::new()),
            conversation: agent_types::ConversationSnapshot::default(),
            tools: Vec::new(),
            tool_choice: agent_types::ToolChoice::None,
            generation: crate::GenerationConfig::default(),
            reasoning: None,
            provider_options: crate::ProviderOptions::new(),
        }
    }

    #[tokio::test]
    async fn returns_the_validated_terminal_message() {
        let message = message();
        let model = ScriptedModel {
            stream_error: None,
            events: vec![
                ModelEvent::TurnStarted {
                    message_id: message.id.clone(),
                    model: message.model.clone(),
                },
                ModelEvent::TurnFinished {
                    message: message.clone(),
                },
            ],
            capabilities: ModelCapabilities::default(),
        };

        let collected = collect_model_turn(
            &model,
            request(),
            ModelCallContext::new(CancellationToken::new()),
        )
        .await
        .expect("turn should complete");
        assert_eq!(collected, message);
    }

    #[tokio::test]
    async fn preserves_establishment_and_stream_failures() {
        let establishment_error = ModelError::Auth("credential rejected".to_owned());
        let model = ScriptedModel {
            stream_error: Some(establishment_error.clone()),
            events: Vec::new(),
            capabilities: ModelCapabilities::default(),
        };
        assert_eq!(
            collect_model_turn(&model, request(), ModelCallContext::default()).await,
            Err(establishment_error)
        );

        let stream_error = ModelError::Cancelled;
        let model = ScriptedModel {
            stream_error: None,
            events: vec![ModelEvent::TurnFailed {
                error: stream_error.clone(),
            }],
            capabilities: ModelCapabilities::default(),
        };
        assert_eq!(
            collect_model_turn(&model, request(), ModelCallContext::default()).await,
            Err(stream_error)
        );
    }

    #[tokio::test]
    async fn lifecycle_violations_become_protocol_errors() {
        let model = Arc::new(ScriptedModel {
            stream_error: None,
            events: vec![ModelEvent::TurnFinished { message: message() }],
            capabilities: ModelCapabilities::default(),
        });
        let error = collect_model_turn(model.as_ref(), request(), ModelCallContext::default())
            .await
            .expect_err("turn finish before start must fail");
        assert!(matches!(error, ModelError::Protocol(_)));
    }
}
