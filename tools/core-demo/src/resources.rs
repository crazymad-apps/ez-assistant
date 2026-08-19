//! Core Demo 的静态页面位置与真实模型资源装配。

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use agent_core::ModelRequestConfig;
use agent_model::{
    GenerationConfig, ModelAttemptEvent, ModelAttemptObserver, ModelCallContext, ModelCapabilities,
    ModelEventStream, ModelRequest, ModelRetryPolicy, ModelRetryReason, ModelService,
    ModelStreamFuture, ProviderOptions, ReasoningConfig, RetryingModelService,
};
use agent_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, ProtocolAdapter, TransportTimeouts,
};
use agent_types::ToolChoice;
use thiserror::Error;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 128_000;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(1)];
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

pub(crate) fn public_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("public")
}

/// 页面可以展示的非敏感模型连接状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelConnectionState {
    Configured,
    Connecting,
    Connected,
    Failed,
}

impl ModelConnectionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Failed => "failed",
        }
    }
}

/// 模型调用与有限重试的低成本观察投影，不保存请求或响应正文。
#[derive(Default)]
pub(crate) struct ModelObservation {
    connection: AtomicU8,
    logical_calls: AtomicU64,
    attempts: AtomicU64,
    retries_scheduled: AtomicU64,
}

impl ModelObservation {
    fn set_connection(&self, state: ModelConnectionState) {
        self.connection.store(state as u8, Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> ModelObservationSnapshot {
        let connection = match self.connection.load(Ordering::Acquire) {
            value if value == ModelConnectionState::Connecting as u8 => {
                ModelConnectionState::Connecting
            }
            value if value == ModelConnectionState::Connected as u8 => {
                ModelConnectionState::Connected
            }
            value if value == ModelConnectionState::Failed as u8 => ModelConnectionState::Failed,
            _ => ModelConnectionState::Configured,
        };
        ModelObservationSnapshot {
            connection,
            logical_calls: self.logical_calls.load(Ordering::Acquire),
            attempts: self.attempts.load(Ordering::Acquire),
            retries_scheduled: self.retries_scheduled.load(Ordering::Acquire),
        }
    }
}

impl ModelAttemptObserver for ModelObservation {
    fn observe(&self, event: ModelAttemptEvent) {
        match event {
            ModelAttemptEvent::Started { .. } => {
                self.attempts.fetch_add(1, Ordering::AcqRel);
            }
            ModelAttemptEvent::RetryScheduled { .. } => {
                self.retries_scheduled.fetch_add(1, Ordering::AcqRel);
            }
            ModelAttemptEvent::EstablishmentFailed { .. }
            | ModelAttemptEvent::StreamEstablished { .. } => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModelObservationSnapshot {
    pub connection: ModelConnectionState,
    pub logical_calls: u64,
    pub attempts: u64,
    pub retries_scheduled: u64,
}

/// Session Agent 共享的冻结模型能力与请求配置。
pub(crate) struct DemoModelResources {
    pub model: Arc<dyn ModelService>,
    pub model_request: ModelRequestConfig,
    pub provider: String,
    pub model_name: String,
    pub context_window_tokens: u64,
    pub observation: Arc<ModelObservation>,
}

impl DemoModelResources {
    pub(crate) fn from_environment(retry_transient: bool) -> Result<Self, ResourceError> {
        let api_key = required_env("DEEPSEEK_API_KEY")?;
        let base_url =
            optional_env("DEEPSEEK_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let model_name = optional_env("DEEPSEEK_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let context_window_tokens = optional_env("DEEPSEEK_CONTEXT_WINDOW_TOKENS")
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|_| ResourceError::InvalidContextWindow)?
            .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
        if context_window_tokens == 0 {
            return Err(ResourceError::InvalidContextWindow);
        }

        let provider: Arc<dyn ModelService> = Arc::new(
            OpenAiCompatibleService::new(
                base_url,
                BearerCredential::new(api_key),
                model_name.clone(),
                context_window_tokens,
                ProtocolAdapter::deepseek(),
                TransportTimeouts::default(),
            )
            .map_err(|error| ResourceError::Provider(error.to_string()))?,
        );
        let observation = Arc::new(ModelObservation::default());
        let provider = if retry_transient {
            Arc::new(RetryingModelService::with_observer(
                provider,
                retry_policy(),
                observation.clone(),
            )) as Arc<dyn ModelService>
        } else {
            provider
        };
        let model: Arc<dyn ModelService> = Arc::new(ObservedModelService {
            inner: provider,
            observation: observation.clone(),
            attempts_observed_by_retry: retry_transient,
        });

        Ok(Self {
            model,
            model_request: deepseek_model_request_config(),
            provider: "deepseek".to_owned(),
            model_name,
            context_window_tokens,
            observation,
        })
    }

    #[cfg(test)]
    pub(crate) fn offline(model: Arc<dyn ModelService>) -> Self {
        let context_window_tokens = model.context_window_tokens();
        Self {
            model,
            model_request: ModelRequestConfig::default(),
            provider: "deterministic".to_owned(),
            model_name: "deterministic-core-demo".to_owned(),
            context_window_tokens,
            observation: Arc::new(ModelObservation::default()),
        }
    }
}

fn deepseek_model_request_config() -> ModelRequestConfig {
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert(
            "deepseek",
            serde_json::json!({
                "thinking": {"type": "enabled"},
                "reasoning_effort": "high"
            }),
        )
        .expect("static DeepSeek provider options are valid");
    ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: Some(ReasoningConfig { effort: None }),
        provider_options,
    }
}

fn retry_policy() -> ModelRetryPolicy {
    ModelRetryPolicy::new(
        BTreeSet::from([
            ModelRetryReason::Connection,
            ModelRetryReason::Timeout,
            ModelRetryReason::RateLimited,
            ModelRetryReason::Unavailable,
        ]),
        RETRY_DELAYS.to_vec(),
        MAX_RETRY_AFTER,
    )
}

fn required_env(name: &'static str) -> Result<String, ResourceError> {
    optional_env(name).ok_or(ResourceError::MissingCredential)
}

fn optional_env(name: &'static str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

struct ObservedModelService {
    inner: Arc<dyn ModelService>,
    observation: Arc<ModelObservation>,
    attempts_observed_by_retry: bool,
}

impl ModelService for ObservedModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.observation
            .logical_calls
            .fetch_add(1, Ordering::AcqRel);
        if !self.attempts_observed_by_retry {
            self.observation.attempts.fetch_add(1, Ordering::AcqRel);
        }
        self.observation
            .set_connection(ModelConnectionState::Connecting);
        Box::pin(async move {
            match self.inner.stream(request, context).await {
                Ok(stream) => {
                    self.observation
                        .set_connection(ModelConnectionState::Connected);
                    Ok(stream as ModelEventStream)
                }
                Err(error) => {
                    self.observation
                        .set_connection(ModelConnectionState::Failed);
                    Err(error)
                }
            }
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum ResourceError {
    #[error("missing DEEPSEEK_API_KEY; configure it in the process environment or repository .env")]
    MissingCredential,
    #[error("DEEPSEEK_CONTEXT_WINDOW_TOKENS must be a positive integer")]
    InvalidContextWindow,
    #[error("DeepSeek provider configuration is invalid: {0}")]
    Provider(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_request_explicitly_enables_reasoning() {
        let config = deepseek_model_request_config();
        assert_eq!(config.reasoning, Some(ReasoningConfig { effort: None }));
        assert_eq!(
            config.provider_options.get("deepseek"),
            Some(&serde_json::json!({
                "thinking": {"type": "enabled"},
                "reasoning_effort": "high"
            }))
        );
    }

    #[test]
    fn retry_policy_is_finite_and_explicit() {
        let policy = retry_policy();
        assert_eq!(policy.delays, RETRY_DELAYS);
        assert_eq!(policy.max_retry_after, MAX_RETRY_AFTER);
    }
}
