//! Agent 执行引擎与执行契约。
//!
//! 本 crate 只负责单次 `AgentExecution` 的模型调用、工具调用循环、上下文构建
//! 与执行事件；会话调度、定时任务和配置加载属于 `assistant-runtime`。
//!
//! - [`ExecutionSpec`] / [`ExecutionInput`] / [`ExecutionContext`]：一次执行的
//!   不可变规格、输入与控制面。`ExecutionContext` 三字段必传（含授权闸），
//!   类型层面杜绝"无授权闸"的隐藏默认。
//! - [`AgentExecution`] / [`ExecutionOutcome`] / [`ExecutionControl`]：执行句柄——
//!   `start` 后由单一 tokio 任务驱动 Agent Loop 状态机（预检预算 → 模型 Turn →
//!   AssistantMessage 落账 → 整批 resolve → 逐 valid invocation 授权/执行 →
//!   ToolResult 落账 → 下一轮），
//!   终态 Completed / Failed / Cancelled / CompactionRequired 恰一，完成结果与
//!   终态事件镜像。
//! - [`ExecutionRecorder`] / [`ToolAuthorizer`]：pending/completed 两阶段 tool
//!   exchange 落账与工具授权 SPI，沿用 `ModelService` 的手写 boxed-future 模式。
//! - [`AgentEvent`] / [`AgentEventStream`]：普通观察事件使用 bounded mpsc
//!   （容量 256）+ `try_send`，唯一终态使用独立 oneshot 可靠交付；订阅断开
//!   不影响执行。
//! - [`ExecutionError`] / [`ExecutionBudget`] / [`GuardrailConfig`]：受控终止分类、
//!   显式资源预算与可选启发式检测；工具失败与授权 `Deny` 不是执行错误，预算是
//!   副作用前硬边界，Guardrail 只在显式配置后启用。

mod authorizer;
mod context;
mod engine;
mod error;
mod event;
mod execution;
mod guardrail;
mod input;
mod policy;
mod recorder;
mod spec;

pub use authorizer::{AllowAllAuthorizer, AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
pub use context::ExecutionContext;
pub use error::{BudgetKind, ExecutionError};
pub use event::{AgentEvent, AgentEventStream, ToolCompletionStatus};
pub use execution::{
    AgentExecution, CompactionReason, CompletionFuture, ExecutionControl, ExecutionOutcome,
};
pub use guardrail::{ActiveGuardrailMode, GuardrailCheckConfig, GuardrailConfig, GuardrailKind};
pub use input::ExecutionInput;
pub use policy::{
    ComposedToolAuthorizer, FileToolPolicyAdapter, GeneralToolPolicyAdapter, PolicyEvaluation,
    ShellToolPolicyAdapter, ToolPolicy, TypedPolicyAdapter, TypedToolPolicy,
};
pub use recorder::{
    ConversationDelta, ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture,
};
pub use spec::{ExecutionBudget, ExecutionSpec};

#[cfg(test)]
pub(crate) mod testutil {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    use futures_core::Stream;

    /// 同步驱动一个立即就绪的 Future；测试中的 SPI 实现不允许挂起。
    pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    /// 从流中同步取出下一个事件；测试流不允许挂起。
    pub(crate) fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
        let mut cx = Context::from_waker(Waker::noop());
        match Pin::new(stream).poll_next(&mut cx) {
            Poll::Ready(item) => item,
            Poll::Pending => panic!("test stream must never pend"),
        }
    }
}
