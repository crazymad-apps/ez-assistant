//! Assistant 应用运行时。
//!
//! Runtime 持有业务权威状态，并负责协调多会话、Agent Run 和配置。正式产品由独立
//! Runtime Host 进程装配本 crate；本 crate 不依赖 Tauri 或具体进程通信方式。

pub mod config;
mod error;
mod factory;
mod id;
mod journal;
mod run;
mod runtime;
mod session;
mod storage;

pub use config::{
    ConfigCompilation, ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigSourceFailure,
    ConfigSourceFailureKind, ConfigSourceFuture, ConfigSourceLoad, ConfigState,
    ModelConfigProjection, ModelProtocol, ResolvedConfig, ResolvedModelConfig, RuntimeConfig,
    RuntimeConfigSource, RuntimeModelTransportConfig, compile_runtime_config,
};
pub use error::{RuntimeError, RuntimeResult};
pub use factory::{
    ModelCompatibilityProfile, ModelServiceFactory, ModelServiceFactoryError,
    ModelServiceFactoryRequest, SystemPromptFactory, SystemPromptFactoryError,
};
pub use runtime::AssistantRuntime;
pub use storage::{
    AcceptedInput, ArchiveChange, CompletedToolExchange, ConversationRewrite, ModelChange,
    NewStoredInput, NewStoredRunAttempt, NewStoredSession, PendingToolExchange, RecoveredRuntime,
    RewriteResult, RuntimeStore, StoreError, StoreErrorKind, StoreFuture, StoredConversationState,
    StoredInput, StoredInputState, StoredRun, StoredRunSettlement, StoredSession,
    StoredSessionLifecycle, UserMessageCommit,
};
