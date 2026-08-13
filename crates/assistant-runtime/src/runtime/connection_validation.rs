//! 显式模型连接验证的固定请求、流消费与安全结果投影。

use std::sync::Arc;

use agent_model::{
    GenerationConfig, LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest,
    ModelService, ModelTransportErrorKind, SystemPromptSnapshot,
};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, ToolChoice,
    UserMessage, UserPart,
};
use assistant_protocol::{
    ConnectionValidationFailure, ConnectionValidationFailureKind, ConnectionValidationOutcome,
    ValidateModelConnectionRequest, ValidateModelConnectionResult,
};
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{AssistantRuntime, model::profile_request_options};
use crate::{RuntimeError, RuntimeResult};

pub(super) const CONNECTION_VALIDATION_PROMPT: &str = "Reply with OK.";
pub(super) const CONNECTION_VALIDATION_MAX_OUTPUT_TOKENS: u32 = 16;

impl AssistantRuntime {
    /// 使用固定最小请求显式验证指定模型，不创建任何 Session/Run 事实。
    pub async fn validate_model_connection(
        &self,
        request: ValidateModelConnectionRequest,
    ) -> RuntimeResult<ValidateModelConnectionResult> {
        self.ensure_running()?;
        let snapshot = self.config_registry.snapshot()?;
        let compiled = match self.compile_model_service(&snapshot, &request.model_key) {
            Ok(compiled) => compiled,
            // 已有有效模型配置但 Host 无法构造服务，属于本次连接验证结果；
            // 顶层配置不可用、key 不存在或条目无效仍使用 Runtime 结构化错误。
            Err(RuntimeError::ModelBuildFailed { .. }) => {
                return Ok(validation_failed(
                    request.model_key,
                    ConnectionValidationFailureKind::Configuration,
                ));
            }
            Err(error) => return Err(error),
        };
        let model_request =
            connection_validation_request(compiled.profile, compiled.max_output_tokens)?;
        let cancellation = self.root_cancellation.child_token();
        let validation =
            consume_validation_stream(compiled.model, model_request, cancellation.clone());
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ModelError::Cancelled),
            // 连接验证是一次显式、最小且无工具的探测，仍复用 request timeout 作为
            // 整体上限；正式模型流由 Transport 将同一配置解释为逐 chunk 空闲上限。
            result = tokio::time::timeout(compiled.request_timeout, validation) => {
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        cancellation.cancel();
                        Err(ModelError::Transport {
                            kind: ModelTransportErrorKind::Timeout,
                            message: "connection validation timed out".to_owned(),
                        })
                    }
                }
            }
        };

        match outcome {
            Ok(()) => Ok(ValidateModelConnectionResult {
                model_key: request.model_key,
                outcome: ConnectionValidationOutcome::Succeeded,
            }),
            Err(ModelError::Cancelled) => Err(RuntimeError::RuntimeNotRunning {
                lifecycle: self.lifecycle()?,
            }),
            Err(error) => {
                let kind = connection_validation_failure_kind(&error);
                Ok(validation_failed(request.model_key, kind))
            }
        }
    }
}

/// 构造不携带 Session、System Prompt、工具或用户数据的固定最小请求。
fn connection_validation_request(
    profile: crate::ModelCompatibilityProfile,
    model_max_output_tokens: u32,
) -> RuntimeResult<ModelRequest> {
    let (reasoning, provider_options) = profile_request_options(profile)?;
    let user_message = UserMessage {
        id: MessageId::new("connection-validation-message")
            .expect("static validation message id is valid"),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("connection-validation-part")
                .expect("static validation part id is valid"),
            text: CONNECTION_VALIDATION_PROMPT.to_owned(),
        })],
    };
    Ok(ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(user_message)]),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig {
            temperature: None,
            top_p: None,
            max_output_tokens: Some(
                model_max_output_tokens.min(CONNECTION_VALIDATION_MAX_OUTPUT_TOKENS),
            ),
            stop: Vec::new(),
        },
        reasoning,
        provider_options,
    })
}

/// 验证流只识别规范终态，不保存 delta、usage 或模型回复正文。
async fn consume_validation_stream(
    model: Arc<dyn ModelService>,
    request: ModelRequest,
    cancellation: CancellationToken,
) -> Result<(), ModelError> {
    let stream = model
        .stream(request, ModelCallContext::new(cancellation))
        .await?;
    let mut stream = LifecycleValidator::new(stream);
    while let Some(event) = stream.next().await {
        match event {
            ModelEvent::TurnFinished { .. } => return Ok(()),
            ModelEvent::TurnFailed { error } => return Err(error),
            _ => {}
        }
    }
    Err(ModelError::Protocol(
        "model stream ended without a terminal event".to_owned(),
    ))
}

/// 只根据 `ModelError` 的结构化事实分类，不解析 Provider 消息文本。
fn connection_validation_failure_kind(error: &ModelError) -> ConnectionValidationFailureKind {
    match error {
        ModelError::Config(_) => ConnectionValidationFailureKind::Configuration,
        ModelError::Auth(_) => ConnectionValidationFailureKind::Authentication,
        ModelError::Transport {
            kind: ModelTransportErrorKind::Timeout,
            ..
        } => ConnectionValidationFailureKind::Timeout,
        ModelError::Transport { .. } => ConnectionValidationFailureKind::Connection,
        ModelError::Provider { status, .. } => match status {
            Some(400 | 404) => ConnectionValidationFailureKind::ModelUnavailable,
            Some(401 | 403) => ConnectionValidationFailureKind::Authentication,
            Some(408) => ConnectionValidationFailureKind::Timeout,
            Some(425 | 429) => ConnectionValidationFailureKind::RateLimited,
            Some(500..=599) => ConnectionValidationFailureKind::ServiceUnavailable,
            _ => ConnectionValidationFailureKind::ProviderRejected,
        },
        ModelError::RateLimited { .. } => ConnectionValidationFailureKind::RateLimited,
        ModelError::Unavailable { .. } => ConnectionValidationFailureKind::ServiceUnavailable,
        ModelError::ContextOverflow { .. } => ConnectionValidationFailureKind::ModelUnavailable,
        ModelError::Protocol(_) | ModelError::ToolArguments(_) | ModelError::Cancelled => {
            ConnectionValidationFailureKind::Protocol
        }
    }
}

fn validation_failed(
    model_key: assistant_protocol::ModelKey,
    kind: ConnectionValidationFailureKind,
) -> ValidateModelConnectionResult {
    let message = match kind {
        ConnectionValidationFailureKind::Configuration => {
            "model connection validation could not be configured"
        }
        ConnectionValidationFailureKind::Connection => "could not connect to the model provider",
        ConnectionValidationFailureKind::Timeout => "model connection validation timed out",
        ConnectionValidationFailureKind::Authentication => {
            "the model provider rejected the configured credential"
        }
        ConnectionValidationFailureKind::ModelUnavailable => "the configured model is unavailable",
        ConnectionValidationFailureKind::RateLimited => {
            "the model provider rate limited the validation request"
        }
        ConnectionValidationFailureKind::ServiceUnavailable => {
            "the model provider is temporarily unavailable"
        }
        ConnectionValidationFailureKind::ProviderRejected => {
            "the model provider rejected the validation request"
        }
        ConnectionValidationFailureKind::Protocol => {
            "the model provider returned an invalid protocol response"
        }
    };
    ValidateModelConnectionResult {
        model_key,
        outcome: ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
            kind,
            message: message.to_owned(),
        }),
    }
}
