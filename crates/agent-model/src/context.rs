use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default)]
/// 单次模型调用的控制面上下文。
///
/// 只承载取消与可选的追踪关联信息。它不是业务 `RunId`（Core 不知道 Runtime 的
/// Run 概念），也不携带 credential；credential 在 Adapter 构造时注入。
pub struct ModelCallContext {
    /// 取消令牌。取消发生后，调用必须产生唯一受控结果：
    /// 建立前以 `Err(ModelError::Cancelled)` 返回，建立后以
    /// `ModelEvent::TurnFailed` 终态结束。
    pub cancellation: CancellationToken,
    /// 可选 trace/correlation 上下文。
    pub trace: Option<TraceContext>,
}

impl ModelCallContext {
    /// 创建只携带取消令牌的调用上下文。
    pub fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            trace: None,
        }
    }

    /// 附加 trace/correlation 上下文。
    pub fn with_trace(mut self, trace: TraceContext) -> Self {
        self.trace = Some(trace);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 跨层日志与诊断的追踪上下文。
pub struct TraceContext {
    /// 调用方生成的关联 ID，用于串联日志；不代表业务 Run。
    pub correlation_id: String,
    /// 当前真实模型调用的 1-based attempt；未装配重试时可以缺省。
    ///
    /// `NonZeroU32` 让 Rust 调用方和 serde 输入都无法构造 attempt 0，同时在 JSON
    /// 中仍序列化为普通数字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<NonZeroU32>,
}

impl TraceContext {
    /// 创建尚未分配 attempt 的追踪上下文。
    pub fn new(correlation_id: impl Into<String>) -> Self {
        Self {
            correlation_id: correlation_id.into(),
            attempt: None,
        }
    }

    /// 返回附带当前 1-based attempt 的新上下文。
    pub fn with_attempt(mut self, attempt: NonZeroU32) -> Self {
        self.attempt = Some(attempt);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_token_observes_cancel() {
        let context = ModelCallContext::default();
        assert!(!context.cancellation.is_cancelled());
        context.cancellation.cancel();
        assert!(context.cancellation.is_cancelled());
    }

    #[test]
    fn trace_context_is_optional_metadata() {
        let context = ModelCallContext::default().with_trace(TraceContext::new("trace_1"));
        assert_eq!(context.trace, Some(TraceContext::new("trace_1")));
    }

    #[test]
    fn trace_context_supports_a_valid_attempt() {
        let trace = TraceContext::new("trace_1")
            .with_attempt(NonZeroU32::new(2).expect("attempt should be non-zero"));
        assert_eq!(trace.attempt.map(NonZeroU32::get), Some(2));

        let json = serde_json::to_value(&trace).expect("trace should serialize");
        assert_eq!(json["attempt"], 2);
        assert_eq!(
            serde_json::from_value::<TraceContext>(json).expect("trace should deserialize"),
            trace
        );
    }

    #[test]
    fn trace_context_accepts_old_json_without_attempt() {
        let trace: TraceContext = serde_json::from_str(r#"{"correlation_id":"trace_1"}"#)
            .expect("old trace JSON should remain readable");
        assert_eq!(trace, TraceContext::new("trace_1"));
        let json = serde_json::to_value(trace).expect("trace should serialize");
        assert!(json.get("attempt").is_none());
    }

    #[test]
    fn trace_context_rejects_attempt_zero() {
        let error =
            serde_json::from_str::<TraceContext>(r#"{"correlation_id":"trace_1","attempt":0}"#)
                .expect_err("attempt zero must be rejected");
        assert!(error.to_string().contains("nonzero"));
    }
}
