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
        let context = ModelCallContext::default().with_trace(TraceContext {
            correlation_id: "trace_1".to_owned(),
        });
        assert_eq!(
            context.trace,
            Some(TraceContext {
                correlation_id: "trace_1".to_owned()
            })
        );
    }
}
