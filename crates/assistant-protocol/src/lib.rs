//! Assistant 各层共享的请求、事件和标识类型。
//!
//! 该 crate 应保持轻量，避免依赖具体 UI、存储或模型实现。

mod command;
mod error;
mod event;
mod id;
mod snapshot;

pub use command::{
    CancelRunRequest, CancelRunResult, CreateSessionRequest, CreateSessionResult, GetRunRequest,
    GetRunResult, GetSessionRequest, GetSessionResult, ListSessionsRequest, ListSessionsResult,
    RuntimeCommand, RuntimeCommandResult, ShutdownRuntimeRequest, ShutdownRuntimeResult,
    StartRunRequest, StartRunResult,
};
pub use error::{RuntimeErrorCode, RuntimeErrorInfo};
pub use event::RuntimeEvent;
pub use id::{IdentifierError, MessageId, PartId, RunId, SessionId, ToolCallId};
pub use snapshot::{
    RunSnapshot, RunStatus, RuntimeLifecycle, SessionSummary, ToolActivitySnapshot,
    ToolActivityStatus, ToolOutputChannel,
};

/// Runtime Host 私有握手所使用的当前应用协议版本。
///
/// 该常量只用于比较双方是否理解同一组业务 DTO，不定义具体传输或 frame 格式。
pub const PROTOCOL_VERSION: u32 = 1;
