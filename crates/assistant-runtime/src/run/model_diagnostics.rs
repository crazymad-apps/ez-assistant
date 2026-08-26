//! 单次 Run 的模型 attempt 观察、脱敏分类与最终失败摘要。

use std::sync::{Mutex, MutexGuard};

use crate::observation::ObservationCoordinator;
use agent_model::{ModelAttemptEvent, ModelAttemptObserver, ModelError, ModelTransportErrorKind};
use assistant_protocol::{ModelFailureKind, RunId, RuntimeEvent, SessionId};

/// 最终结算使用的安全模型失败事实；不保存 Provider 展示文本。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ModelFailureDiagnostics {
    pub(crate) kind: ModelFailureKind,
    pub(crate) attempts: u32,
    pub(crate) retries: u32,
    pub(crate) stream_established: bool,
    pub(crate) output_observed: bool,
}

#[derive(Default)]
struct ModelAttemptState {
    attempt: u32,
    retries: u32,
    stream_established: bool,
    output_observed: bool,
}

/// Runtime 绑定 Session/Run 的 attempt observer，同时维护最终结算所需的最小状态。
pub(crate) struct RunModelDiagnostics {
    session_id: SessionId,
    run_id: RunId,
    events: ObservationCoordinator,
    state: Mutex<ModelAttemptState>,
}

impl RunModelDiagnostics {
    pub(crate) fn new(
        session_id: SessionId,
        run_id: RunId,
        events: ObservationCoordinator,
    ) -> Self {
        Self {
            session_id,
            run_id,
            events,
            state: Mutex::new(ModelAttemptState::default()),
        }
    }

    /// 标记已经有正文或 reasoning 增量进入 Runtime 观察投影。
    pub(crate) fn mark_output_observed(&self) {
        self.lock_state().output_observed = true;
    }

    /// AgentEvent 与 delta 共用同一有序通道，以 StepStarted 清除上一 Step 的可见输出事实。
    pub(crate) fn mark_step_started(&self) {
        self.lock_state().output_observed = false;
    }

    pub(crate) fn failure(&self, error: &ModelError) -> ModelFailureDiagnostics {
        let state = self.lock_state();
        ModelFailureDiagnostics {
            kind: model_failure_kind(error),
            attempts: state.attempt.max(1),
            retries: state.retries,
            stream_established: state.stream_established,
            output_observed: state.output_observed,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, ModelAttemptState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn publish(&self, event: RuntimeEvent) {
        let _ = self.events.send(event);
    }
}

impl ModelAttemptObserver for RunModelDiagnostics {
    fn observe(&self, event: ModelAttemptEvent) {
        match event {
            ModelAttemptEvent::Started { attempt, .. } => {
                {
                    let mut state = self.lock_state();
                    if attempt == 1 {
                        *state = ModelAttemptState::default();
                    }
                    state.attempt = attempt;
                }
                self.publish(RuntimeEvent::ModelAttemptStarted {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    attempt,
                });
            }
            ModelAttemptEvent::EstablishmentFailed {
                attempt,
                error,
                will_retry,
                ..
            } => {
                self.lock_state().attempt = attempt;
                self.publish(RuntimeEvent::ModelAttemptFailed {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    attempt,
                    kind: model_failure_kind(&error),
                    will_retry,
                });
            }
            ModelAttemptEvent::RetryScheduled {
                next_attempt,
                delay_ms,
                ..
            } => {
                self.lock_state().retries = next_attempt.saturating_sub(1);
                self.publish(RuntimeEvent::ModelRetryScheduled {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    next_attempt,
                    delay_ms,
                });
            }
            ModelAttemptEvent::StreamEstablished { attempt, .. } => {
                {
                    let mut state = self.lock_state();
                    state.attempt = attempt;
                    state.stream_established = true;
                }
                self.publish(RuntimeEvent::ModelStreamEstablished {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    attempt,
                });
            }
        }
    }
}

pub(crate) fn model_failure_kind(error: &ModelError) -> ModelFailureKind {
    match error {
        ModelError::Config(_) => ModelFailureKind::Configuration,
        ModelError::Auth(_) => ModelFailureKind::Authentication,
        ModelError::Transport {
            kind: ModelTransportErrorKind::Connection,
            ..
        } => ModelFailureKind::Connection,
        ModelError::Transport {
            kind: ModelTransportErrorKind::Timeout,
            ..
        } => ModelFailureKind::Timeout,
        ModelError::Transport {
            kind: ModelTransportErrorKind::Interrupted,
            ..
        } => ModelFailureKind::StreamInterrupted,
        ModelError::Provider { .. } => ModelFailureKind::ProviderRejected,
        ModelError::RateLimited { .. } => ModelFailureKind::RateLimited,
        ModelError::Unavailable { .. } => ModelFailureKind::ServiceUnavailable,
        ModelError::ContextOverflow { .. } => ModelFailureKind::ContextOverflow,
        ModelError::Protocol(_) => ModelFailureKind::Protocol,
        ModelError::ToolArguments(_) => ModelFailureKind::ToolArguments,
        ModelError::Resource(_) => ModelFailureKind::Resource,
        ModelError::Cancelled => ModelFailureKind::Cancelled,
    }
}

pub(crate) fn model_failure_kind_value(kind: ModelFailureKind) -> &'static str {
    match kind {
        ModelFailureKind::Configuration => "configuration",
        ModelFailureKind::Authentication => "authentication",
        ModelFailureKind::Connection => "connection",
        ModelFailureKind::Timeout => "timeout",
        ModelFailureKind::StreamInterrupted => "stream_interrupted",
        ModelFailureKind::ProviderRejected => "provider_rejected",
        ModelFailureKind::RateLimited => "rate_limited",
        ModelFailureKind::ServiceUnavailable => "service_unavailable",
        ModelFailureKind::ContextOverflow => "context_overflow",
        ModelFailureKind::Protocol => "protocol",
        ModelFailureKind::ToolArguments => "tool_arguments",
        ModelFailureKind::Resource => "resource",
        ModelFailureKind::Cancelled => "cancelled",
    }
}
