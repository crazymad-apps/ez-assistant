//! Provider-neutral 的有限模型重试。建流与流中失败共用预算；
//! 瞬态重试从原请求恢复；显式启用的参数纠错只追加请求期反馈。
//! TurnRestarted 明确废弃尚未提交的响应，两条路径均不执行工具。

use std::{collections::BTreeSet, num::NonZeroU32, sync::Arc, time::Duration};

use agent_types::{
    ContextInsertionPayload, ContextInsertionPlan, ConversationMessage, InternalContextPart,
    MessageId, PartId, TranscriptVisibility, UserMessage, UserMessageOrigin, UserPart,
};
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
    /// 单次逻辑调用允许的工具参数纠错次数；仍消耗 delays 的总 attempt 预算。
    /// 0 表示关闭，且只接受流内 ToolArguments，不处理编码请求时的错误。
    pub max_tool_argument_corrections: u32,
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
            max_tool_argument_corrections: 0,
        }
    }

    /// 显式允许把工具参数解析错误反馈给模型，重新生成尚未提交的响应。
    /// 不补写参数、不伪造 ToolResult；原始规范历史和冻结 system 保持不变。
    pub fn with_tool_argument_corrections(mut self, limit: u32) -> Self {
        self.max_tool_argument_corrections = limit;
        self
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

/// 按显式预算重试瞬态错误、反馈工具参数错误的 [`ModelService`] 装饰器。
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
                tool_argument_corrections: 0,
                feedback: None,
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
    tool_argument_corrections: u32,
    /// 仅进入下一 attempt 的上下文；成功响应中不保留，不能写回规范历史。
    feedback: Option<ConversationMessage>,
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
            let mut request = self.request.clone();
            if let Some(feedback) = &self.feedback {
                request.conversation.messages.push(feedback.clone());
            }
            let result = tokio::select! {
                biased;
                () = self.context.cancellation.cancelled() => Err(ModelError::Cancelled),
                result = self.service.inner.stream(request, context) => result,
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
        // 请求编码阶段的 ToolArguments 可能来自历史损坏，不能循环重发。
        // 仅重新采样尚未完成的当前响应，仍受同一 attempt 总额与取消约束。
        let correction = in_stream
            && matches!(error, ModelError::ToolArguments(_))
            && !self.request.tools.is_empty()
            && self.tool_argument_corrections < self.service.policy.max_tool_argument_corrections;
        let will_retry = !self.context.cancellation.is_cancelled()
            && next_attempt.is_some()
            && delay.is_some()
            && (correction
                || reason.is_some_and(|reason| self.service.policy.retry_on.contains(&reason)))
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
        if correction {
            self.tool_argument_corrections += 1;
            self.feedback = Some(tool_argument_feedback(&self.request, error));
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

/// 失败响应从未可靠完成、也未进入工具执行。把已脱敏错误作为文本反馈，
/// 不把坏 arguments 放回 tool_calls，不改写 system 或规范 Conversation。
/// 复用请求期内插计划；重复纠错替换同一条临时消息，避免上下文无限累积。
fn tool_argument_feedback(request: &ModelRequest, error: &ModelError) -> ConversationMessage {
    let diagnostic: String = error.to_string().chars().take(1024).collect();
    let text = format!(
        "The previous model response was discarded because a tool call had invalid JSON arguments. \
         No tool calls from that response were executed. Previously completed tool results in the history remain valid; do not repeat them. \
         Continue the original task and regenerate the unexecuted tool calls with complete, valid JSON matching their schemas. \
         Do not guess missing argument values. Parser diagnostic (quoted data): {}",
        serde_json::to_string(&diagnostic).expect("serializing a string cannot fail"),
    );
    let mut identity = "tool_argument_correction".to_owned();
    while request.conversation.messages.iter().any(|message| {
        let id = match message {
            ConversationMessage::User(message) => &message.id,
            ConversationMessage::Assistant(message) => &message.id,
            ConversationMessage::Tool(message) => &message.id,
            ConversationMessage::System(message) => &message.id,
            ConversationMessage::ContextSummary(message) => &message.id,
        };
        id.as_str() == identity
    }) {
        identity.push('_');
    }
    let part = InternalContextPart::new(
        PartId::new(identity.clone()).expect("correction identity is non-empty"),
        identity.clone(),
        "tool_argument_correction",
        text,
    )
    .expect("correction context is non-empty");
    let plan = ContextInsertionPlan::request_only_internal(identity.clone(), part);
    let ContextInsertionPayload::InternalContext(part) = plan.payload else {
        unreachable!("internal context plan carries internal context")
    };
    ConversationMessage::User(UserMessage {
        id: MessageId::new(identity).expect("correction identity is non-empty"),
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        parts: vec![UserPart::InternalContext(part)],
    })
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
