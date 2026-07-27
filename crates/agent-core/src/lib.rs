//! Agent 执行引擎与执行契约。
//!
//! 本 crate 只负责单次 `AgentExecution` 的模型调用、工具调用循环、上下文构建
//! 与执行事件；会话调度、定时任务和配置加载属于 `assistant-runtime`。
//!
//! - [`ExecutionSpec`] / [`ExecutionInput`] / [`ExecutionContext`]：一次执行的
//!   不可变规格、输入与控制面。`ExecutionContext` 三字段必传（含授权闸），
//!   类型层面杜绝"无授权闸"的隐藏默认。
//! - [`ExecutionRecorder`] / [`ToolAuthorizer`]：规范对话落账与工具授权 SPI，
//!   沿用 `ModelService` 的手写 boxed-future 模式（对象安全，无 async-trait）。
//! - [`AgentEvent`] / [`AgentEventStream`]：执行事件与背压通道——bounded mpsc
//!   （容量 [`AGENT_EVENT_CHANNEL_CAPACITY`]）+ `try_send`，满了丢弃并计数，
//!   订阅断开不影响执行。
//! - [`ExecutionError`] / [`ExecutionBudget`]：受控终止分类与显式资源预算；
//!   工具失败与授权 `Deny` 不是执行错误，预算是副作用前的硬边界。
//!
//! 引擎状态机（`AgentExecution`）在后续里程碑落地。

mod authorizer;
mod context;
mod error;
mod event;
mod input;
mod recorder;
mod spec;

pub use authorizer::{AllowAllAuthorizer, AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
pub use context::ExecutionContext;
pub use error::{BudgetKind, ExecutionError};
pub use event::{
    AGENT_EVENT_CHANNEL_CAPACITY, AgentEvent, AgentEventSender, AgentEventStream,
    ToolCompletionStatus, agent_event_channel,
};
pub use input::ExecutionInput;
pub use recorder::{
    ConversationDelta, ExecutionRecorder, RecordError, RecordFuture, RecordReceipt,
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
