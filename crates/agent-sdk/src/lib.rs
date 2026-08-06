//! Agent Core 的候选便利装配入口。
//!
//! 本 crate 用 [`AgentBuilder`] 把冻结的模型、System Prompt、Context Window、
//! 工具与执行配置收敛为 [`Agent`]。每次执行仍由
//! [`agent_core::AgentExecution::start`] 驱动唯一的 Core 状态机；SDK 不拥有 Session、
//! Run、Conversation、Journal、审批状态、持久化或调度。
//!
//! 普通宿主可以选择两条执行路径：
//!
//! - [`Agent::start`]：显式传入 Runtime 风格的 Recorder 与 Authorizer；
//! - [`Agent::start_ephemeral`]：使用一次执行独享、不可恢复的临时 Recorder。

mod agent;
mod builder;
mod ephemeral_recorder;
mod error;

pub use agent::Agent;
pub use builder::AgentBuilder;
pub use error::AgentBuildError;

pub use agent_context::ContextWindowEvaluator;
pub use agent_core::{
    AgentExecution, AllowAllAuthorizer, ExecutionBudget, ExecutionContext, ExecutionInput,
    ExecutionOutcome, ExecutionRecorder, ModelRequestConfig, ToolAuthorizer,
};
pub use agent_model::{ModelService, SystemPromptSnapshot};
pub use agent_tools::ToolSetSnapshot;
