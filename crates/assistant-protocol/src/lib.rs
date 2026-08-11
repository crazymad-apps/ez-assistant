//! Assistant 各层共享的请求、事件和标识类型。
//!
//! 该 crate 应保持轻量，避免依赖具体 UI、存储或模型实现。

mod command;
mod config;
mod error;
mod event;
mod host;
mod id;
mod snapshot;

pub use command::{
    ArchiveSessionRequest, ArchiveSessionResult, CancelQueuedInputRequest, CancelQueuedInputResult,
    CancelRunRequest, CancelRunResult, ConnectionValidationFailure,
    ConnectionValidationFailureKind, ConnectionValidationOutcome, CreateSessionRequest,
    CreateSessionResult, GetAttachmentRequest, GetAttachmentResult, GetConfigStatusRequest,
    GetConfigStatusResult, GetModelRequest, GetModelResult, GetRunRequest, GetRunResult,
    GetSessionRequest, GetSessionResult, GetWorkspaceRequest, GetWorkspaceResult,
    ListAttachmentsRequest, ListAttachmentsResult, ListModelsRequest, ListModelsResult,
    ListRunsRequest, ListRunsResult, ListSessionsRequest, ListSessionsResult,
    ListWorkspacesRequest, ListWorkspacesResult, ReenterFromUserMessageRequest,
    ReenterFromUserMessageResult, RegisterWorkspaceRequest, RegisterWorkspaceResult,
    ReloadConfigRequest, ReloadConfigResult, RemoveWorkspaceRequest, RemoveWorkspaceResult,
    RestoreSessionRequest, RestoreSessionResult, ResumeSessionRequest, ResumeSessionResult,
    RetryRunRequest, RetryRunResult, RuntimeCommand, RuntimeCommandResult, SetSessionModelRequest,
    SetSessionModelResult, ShutdownRuntimeRequest, ShutdownRuntimeResult, SubmitInputRequest,
    SubmitInputResult, UploadAttachmentResult, ValidateModelConnectionRequest,
    ValidateModelConnectionResult,
};
pub use config::{
    ConfigurationIssue, ConfigurationIssueCode, ConfigurationState, ConfigurationStatus,
    ModelConfiguration,
};
pub use error::{ModelFailureKind, RuntimeErrorCode, RuntimeErrorInfo};
pub use event::RuntimeEvent;
pub use host::{RuntimeHostCapabilities, RuntimeHostHealth, RuntimeHostHealthStatus};
pub use id::{
    AttachmentId, IdempotencyKey, IdentifierError, InputId, MessageId, ModelKey, ModelKeyError,
    PartId, RunId, SessionId, ToolCallId, WorkspaceId,
};
pub use snapshot::{
    AttachmentState, AttachmentSummary, RunSnapshot, RunStatus, RuntimeLifecycle, SessionLifecycle,
    SessionListFilter, SessionSummary, TokenUsageSnapshot, ToolActivitySnapshot,
    ToolActivityStatus, ToolOutputChannel, WorkspaceLifecycle, WorkspaceSummary,
};

/// 客户端与 Runtime Host 当前共同理解的应用协议版本。
///
/// 该常量通过 Host capabilities 投影，不定义 HTTP 或 SSE 的传输版本。
pub const PROTOCOL_VERSION: u32 = 1;
