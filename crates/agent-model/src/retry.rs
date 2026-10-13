//! Provider-neutral 的有限模型重试。建流与流中失败共用预算；
//! 重试从原请求恢复，TurnRestarted 明确废弃尚未提交的响应。

use std::{collections::BTreeSet, num::NonZeroU32, sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};

use crate::{
    ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelService, ModelStreamFuture,
    ModelTransportErrorKind, TraceContext,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 上层策略可以显式允许的瞬态错误原因。
pub enum ModelRetryReason {
    /// 连接建立失败或响应流中断。
    Connection,
    /// 连接或整体请求超时。
    Timeout,
    /// Provider 对当前请求限流。
    RateLimited,
    /// Provider 当前暂时不可用。
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一次 [`RetryingModelService`] 的显式有限重试策略。
///
/// `delays[n]` 是第 `n + 1` 次失败后、下一 attempt 前的等待，因此最大 attempt
/// 恒为 `delays.len() + 1`。本类型没有 `Default`，避免无意启用隐藏重试。
pub struct ModelRetryPolicy {
    /// 允许重试的稳定错误原因集合。
    pub retry_on: BTreeSet<ModelRetryReason>,
    /// 每次重试前的显式等待表。
    pub delays: Vec<Duration>,
    /// 接受 Provider `Retry-After` 的最大值；超过时直接返回原错误。
    pub max_retry_after: Duration,
}

impl ModelRetryPolicy {
    /// 创建一个完全显式的有限重试策略。
    pub fn new(
        retry_on: BTreeSet<ModelRetryReason>,
        delays: Vec<Duration>,
        max_retry_after: Duration,
    ) -> Self {
        Self {
            retry_on,
            delays,
            max_retry_after,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 一个逻辑模型调用内部的 attempt 生命周期事实。
pub enum ModelAttemptEvent {
    /// 即将调用一次真实下层 ModelService。
    Started {
        /// 当前调用关联信息；存在时已写入当前 attempt。
        trace: Option<TraceContext>,
        /// 当前 1-based attempt。
        attempt: u32,
    },
    /// 下层在返回事件流前失败。
    EstablishmentFailed {
        /// 当前调用关联信息；存在时已写入当前 attempt。
        trace: Option<TraceContext>,
        /// 当前 1-based attempt。
        attempt: u32,
        /// 原始规范模型错误。
        error: ModelError,
        /// 从结构化错误得到的可选瞬态原因。
        retry_reason: Option<ModelRetryReason>,
        /// 当前策略与取消状态是否允许安排下一 attempt。
        will_retry: bool,
    },
    /// 下层事件流以受控失败结束。
    StreamFailed {
        /// 当前调用关联信息；存在时已写入当前 attempt。
        trace: Option<TraceContext>,
        /// 当前 1-based attempt。
        attempt: u32,
        /// 原始规范模型错误。
        error: ModelError,
        /// 从结构化错误得到的可选瞬态原因。
        retry_reason: Option<ModelRetryReason>,
        /// 当前策略与取消状态是否允许安排下一 attempt。
        will_retry: bool,
    },
    /// 已确定下一 attempt 及实际等待时间。
    RetryScheduled {
        /// 下一 attempt 的关联信息。
        trace: Option<TraceContext>,
        /// 即将执行的 1-based attempt。
        next_attempt: u32,
        /// 策略等待与 `Retry-After` 合并后的实际毫秒数。
        delay_ms: u64,
    },
    /// 下层已成功返回事件流，随后仍监视受控流中失败。
    StreamEstablished {
        /// 当前调用关联信息；存在时已写入当前 attempt。
        trace: Option<TraceContext>,
        /// 成功建立流的 1-based attempt。
        attempt: u32,
    },
}

/// 模型 attempt 事实的同步观察接口。
///
/// 实现应只做快速、非阻塞、非 panic 的队列投递。接口不返回结果，观察数据丢弃
/// 不能改变模型调用结果。
pub trait ModelAttemptObserver: Send + Sync {
    /// 接收一个 attempt 生命周期事实。
    fn observe(&self, event: ModelAttemptEvent);
}

/// 重试建流与流中瞬态错误的 [`ModelService`] 装饰器。
#[derive(Clone)]
pub struct RetryingModelService {
    inner: Arc<dyn ModelService>,
    policy: ModelRetryPolicy,
    observer: Option<Arc<dyn ModelAttemptObserver>>,
}

impl RetryingModelService {
    /// 创建不带观察器的显式重试包装器。
    pub fn new(inner: Arc<dyn ModelService>, policy: ModelRetryPolicy) -> Self {
        Self {
            inner,
            policy,
            observer: None,
        }
    }

    /// 创建带 attempt 观察器的显式重试包装器。
    pub fn with_observer(
        inner: Arc<dyn ModelService>,
        policy: ModelRetryPolicy,
        observer: Arc<dyn ModelAttemptObserver>,
    ) -> Self {
        Self {
            inner,
            policy,
            observer: Some(observer),
        }
    }

    fn observe(&self, event: ModelAttemptEvent) {
        if let Some(observer) = &self.observer {
            observer.observe(event);
        }
    }
}

impl ModelService for RetryingModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn max_input_tokens(&self) -> Option<u64> {
        self.inner.max_input_tokens()
    }

    fn context_policy(&self) -> &crate::ContextPolicy {
        self.inner.context_policy()
    }

    fn count_input_tokens<'a>(
        &'a self,
        request: &'a crate::ModelRequest,
        context: &'a mut crate::ModelCallContext,
    ) -> crate::ModelInputTokenCountFuture<'a> {
        self.inner.count_input_tokens(request, context)
    }

    fn compact(
        &self,
        request: crate::ModelRequest,
        context: crate::ModelCallContext,
    ) -> crate::ModelCompactionFuture<'_> {
        self.inner.compact(request, context)
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let service = self.clone();
        Box::pin(async move {
            let mut state = RetryStream {
                service,
                request,
                context,
                attempt: 1,
                stream: None,
                done: false,
            };
            state.establish().await?;
            Ok(Box::pin(futures_util::stream::unfold(
                state,
                |mut state| async move {
                    let event = state.next_event().await?;
                    Some((event, state))
                },
            )) as crate::ModelEventStream)
        })
    }
}

/// 一个逻辑调用对建流与流中失败共用有限预算。
struct RetryStream {
    service: RetryingModelService,
    request: ModelRequest,
    context: ModelCallContext,
    attempt: u32,
    stream: Option<crate::ModelEventStream>,
    done: bool,
}

impl RetryStream {
    async fn next_event(&mut self) -> Option<crate::ModelEvent> {
        if self.done {
            return None;
        }
        let event = tokio::select! {
            biased;
            () = self.context.cancellation.cancelled() => crate::ModelEvent::TurnFailed { error: ModelError::Cancelled },
            event = self.stream.as_mut().expect("established stream").next() => {
                event.expect("validator supplies a terminal event")
            }
        };
        if let crate::ModelEvent::TurnFailed { error } = event {
            // 等待重连前先释放失败流；原请求只包含已经可靠提交的上下文。
            self.stream = None;
            match self.retry(&error).await {
                Ok(()) => return Some(crate::ModelEvent::TurnRestarted),
                Err(error) => {
                    self.done = true;
                    return Some(crate::ModelEvent::TurnFailed { error });
                }
            }
        }
        if event.is_terminal() {
            self.done = true;
        }
        Some(event)
    }

    async fn establish(&mut self) -> Result<(), ModelError> {
        loop {
            if self.context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let trace = trace_for_attempt(self.context.trace.as_ref(), self.attempt);
            self.service.observe(ModelAttemptEvent::Started {
                trace: trace.clone(),
                attempt: self.attempt,
            });
            let mut context = self.context.clone();
            context.trace = trace.clone();
            let result = tokio::select! {
                biased;
                () = self.context.cancellation.cancelled() => Err(ModelError::Cancelled),
                result = self.service.inner.stream(self.request.clone(), context) => result,
            };
            match result {
                Ok(stream) => {
                    self.service.observe(ModelAttemptEvent::StreamEstablished {
                        trace,
                        attempt: self.attempt,
                    });
                    self.stream = Some(Box::pin(crate::LifecycleValidator::new(stream)));
                    return Ok(());
                }
                Err(error) => self.schedule_retry(&error, false).await?,
            }
        }
    }

    async fn retry(&mut self, error: &ModelError) -> Result<(), ModelError> {
        self.schedule_retry(error, true).await?;
        self.establish().await
    }

    async fn schedule_retry(
        &mut self,
        error: &ModelError,
        in_stream: bool,
    ) -> Result<(), ModelError> {
        let reason = retry_reason(error);
        let delay = self
            .service
            .policy
            .delays
            .get(self.attempt as usize - 1)
            .copied();
        let suggested = retry_after(error);
        let next_attempt = self.attempt.checked_add(1);
        let will_retry = !self.context.cancellation.is_cancelled()
            && next_attempt.is_some()
            && delay.is_some()
            && reason.is_some_and(|reason| self.service.policy.retry_on.contains(&reason))
            && suggested.is_none_or(|delay| delay <= self.service.policy.max_retry_after);
        let trace = trace_for_attempt(self.context.trace.as_ref(), self.attempt);
        let event = if in_stream {
            ModelAttemptEvent::StreamFailed {
                trace,
                attempt: self.attempt,
                error: error.clone(),
                retry_reason: reason,
                will_retry,
            }
        } else {
            ModelAttemptEvent::EstablishmentFailed {
                trace,
                attempt: self.attempt,
                error: error.clone(),
                retry_reason: reason,
                will_retry,
            }
        };
        self.service.observe(event);
        if self.context.cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        if !will_retry {
            return Err(error.clone());
        }
        let next_attempt = next_attempt.expect("retry requires next attempt");
        let configured = delay.expect("retry requires configured delay");
        let effective = suggested.map_or(configured, |delay| configured.max(delay));
        self.service.observe(ModelAttemptEvent::RetryScheduled {
            trace: trace_for_attempt(self.context.trace.as_ref(), next_attempt),
            next_attempt,
            delay_ms: duration_millis(effective),
        });
        tokio::select! {
            biased;
            () = self.context.cancellation.cancelled() => return Err(ModelError::Cancelled),
            () = tokio::time::sleep(effective) => {}
        }
        self.attempt = next_attempt;
        Ok(())
    }
}

/// 为当前 attempt 复制逻辑调用 Trace；没有 correlation 时继续保持 `None`。
fn trace_for_attempt(trace: Option<&TraceContext>, attempt: u32) -> Option<TraceContext> {
    let attempt = NonZeroU32::new(attempt).expect("attempt is always 1-based");
    trace.cloned().map(|trace| trace.with_attempt(attempt))
}

/// 只从结构化错误提取允许进入策略判断的稳定原因。
fn retry_reason(error: &ModelError) -> Option<ModelRetryReason> {
    match error {
        ModelError::Transport {
            kind: ModelTransportErrorKind::Connection | ModelTransportErrorKind::Interrupted,
            ..
        } => Some(ModelRetryReason::Connection),
        ModelError::Transport {
            kind: ModelTransportErrorKind::Timeout,
            ..
        } => Some(ModelRetryReason::Timeout),
        ModelError::RateLimited { .. } => Some(ModelRetryReason::RateLimited),
        ModelError::Unavailable { .. } => Some(ModelRetryReason::Unavailable),
        _ => None,
    }
}

/// Provider 明确给出的建议等待时间；普通错误没有该事实。
fn retry_after(error: &ModelError) -> Option<Duration> {
    match error {
        ModelError::RateLimited { retry_after_ms, .. }
        | ModelError::Unavailable { retry_after_ms, .. } => {
            retry_after_ms.map(Duration::from_millis)
        }
        _ => None,
    }
}

/// attempt 事件使用 u64 毫秒；极端 Duration 饱和而不截断回绕。
fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
