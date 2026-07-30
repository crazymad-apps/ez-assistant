//! Provider-neutral 的单次模型调用（Provider Turn）契约。
//!
//! 这个 crate 定义一次模型调用的稳定边界，不知道具体 API Key 来源、配置文件
//! 位置和应用 Session：
//!
//! - [`ModelService`]：单次 Turn 服务。一次只执行一个 Provider Turn，不执行工具，
//!   也不会在收到 Tool Call 后自动继续；是否继续由 Agent Engine 显式决定。
//! - [`ModelRequest`] / [`ModelCallContext`]：语义输入与控制面分离。请求和上下文
//!   都不携带业务 `RunId` 与 credential；credential 在 Adapter 构造时注入，
//!   不进入请求、上下文、事件和 Debug 输出。
//! - [`ModelEvent`] / [`ModelEventStream`]：规范流式事件。建立前失败由
//!   [`ModelStreamFuture`] 返回 `Err`；流建立后的失败以 `TurnFailed` 受控终态结束。
//! - [`LifecycleValidator`]：强制执行 Part 生命周期配对与唯一终态，
//!   不依赖任何具体 Provider。
//! - [`ModelError`]：配置、认证、Transport、Provider、限流、Context Overflow、
//!   协议、Tool arguments 和取消的分类，只携带脱敏诊断信息。
//!
//! 最小调用示例见 `service.rs` 内联测试：`FakeModelService` 演示了通过
//! [`ModelService`] SPI 完成一次 Turn 的完整路径；真实 Adapter 的端到端调用见
//! `agent-provider-openai-compatible` 的 `examples/chat.rs`。

mod capabilities;
mod context;
mod error;
mod event;
mod lifecycle;
mod request;
mod service;

pub use capabilities::ModelCapabilities;
pub use context::{ModelCallContext, TraceContext};
pub use error::ModelError;
pub use event::ModelEvent;
pub use lifecycle::LifecycleValidator;
pub use request::{
    GenerationConfig, ModelRequest, ProviderOptions, ProviderOptionsError, ReasoningConfig,
    ReasoningEffort,
};
pub use service::{ModelEventStream, ModelService, ModelStreamFuture};

#[cfg(test)]
pub(crate) mod testutil {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };

    use futures_core::Stream;
    use futures_util::task::noop_waker_ref;

    use crate::{ModelEvent, ModelEventStream};

    /// 同步驱动一个立即就绪的 Future；测试中的 Fake 不允许挂起。
    pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
        let waker = noop_waker_ref();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    /// 从流中同步取出下一个事件。
    pub(crate) fn next(stream: &mut ModelEventStream) -> Option<ModelEvent> {
        let waker = noop_waker_ref();
        let mut cx = Context::from_waker(waker);
        match stream.as_mut().poll_next(&mut cx) {
            Poll::Ready(event) => event,
            Poll::Pending => panic!("test stream must never pend"),
        }
    }

    /// 同步收集整个流。
    pub(crate) fn collect(mut stream: impl Stream<Item = ModelEvent> + Unpin) -> Vec<ModelEvent> {
        let waker = noop_waker_ref();
        let mut cx = Context::from_waker(waker);
        let mut events = Vec::new();
        loop {
            match Pin::new(&mut stream).poll_next(&mut cx) {
                Poll::Ready(Some(event)) => events.push(event),
                Poll::Ready(None) => return events,
                Poll::Pending => panic!("test stream must never pend"),
            }
        }
    }
}
