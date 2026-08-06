//! Core Demo 私有 HTTP 请求、快照与 SSE DTO。
//!
//! 这些类型只服务验证页面，不进入 `assistant-protocol`。

use std::collections::BTreeMap;

use agent_memory::{MemoryPropertyValue, PinnedMemoryEntry};
use agent_types::ConversationMessage;
use serde::{Deserialize, Serialize};

use crate::{
    approval::{ApprovalDecision, PendingApprovalSnapshot},
    audit::AuditEntry,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionMode {
    Plan,
    Build,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalMode {
    Ask,
    Auto,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    CompactionRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ConfigSnapshot {
    pub workdir: String,
    pub data_dir: String,
    pub provider: String,
    pub model: String,
    pub context_window_tokens: u64,
    pub reasoning_enabled: bool,
    pub retry_transient: bool,
    pub max_compaction_handoffs: u32,
    pub connection_status: String,
    pub model_calls: u64,
    pub model_attempts: u64,
    pub retries_scheduled: u64,
    pub persistence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct SessionSummary {
    pub session_id: String,
    pub title: String,
    pub active_run: bool,
    pub last_status: Option<RunStatus>,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct GlobalSnapshot {
    pub sequence: u64,
    pub config: ConfigSnapshot,
    pub sessions: Vec<SessionSummary>,
    pub memory: MemoryStoreSnapshot,
}

/// Pinned Store 的最新状态；它与任何 Session 的冻结 Prompt 分开显示。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct MemoryStoreSnapshot {
    pub revision: u64,
    pub entries: Vec<PinnedMemoryEntry>,
}

/// Session 创建时冻结的 Prompt 摘要，不向普通页面传输完整 System Prompt。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct FrozenPromptSummary {
    pub part_count: usize,
    pub pinned_revision: u64,
    pub pinned_entry_count: usize,
    pub recall_sources: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct RunSnapshot {
    pub run_id: String,
    pub status: RunStatus,
    pub execution_mode: ExecutionMode,
    pub approval_mode: ApprovalMode,
    pub cancel_requested: bool,
    pub event_count: u64,
    pub dropped_events: u64,
    pub guardrail_triggers: u64,
    pub compaction_handoffs: u32,
    pub reasoning: String,
    pub text: String,
    pub last_event: Option<String>,
    pub last_error: Option<String>,
    pub tools: Vec<ToolActivitySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ToolActivitySnapshot {
    pub call_id: String,
    pub tool_name: String,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(crate) struct SessionSnapshot {
    pub session_id: String,
    pub title: String,
    pub sequence: u64,
    pub active_run: bool,
    pub pending_exchange: bool,
    pub temporary_workspace: String,
    pub frozen_prompt: FrozenPromptSummary,
    pub journal: Vec<ConversationMessage>,
    pub run: Option<RunSnapshot>,
    pub approval: Option<PendingApprovalSnapshot>,
    pub audit: Vec<AuditEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct PinMemoryRequest {
    pub category: String,
    pub content: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, MemoryPropertyValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct UpdateMemoryRequest {
    pub category: Option<String>,
    pub content: Option<String>,
    pub attributes: Option<BTreeMap<String, MemoryPropertyValue>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct CreateSessionRequest {
    pub title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct StartRunRequest {
    pub message: String,
    pub execution_mode: ExecutionMode,
    pub approval_mode: ApprovalMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct ApprovalDecisionRequest {
    pub decision: ApprovalDecision,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct EventNotification {
    pub sequence: u64,
    pub session_id: String,
    pub session_sequence: u64,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EventKind {
    SessionCreated,
    RunStarted { run_id: String },
    RunProgress { run_id: String, event: String },
    RunCancelRequested { run_id: String },
    ApprovalChanged { run_id: String },
    RunFinished { run_id: String, status: RunStatus },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct GapNotification {
    pub skipped: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ApiErrorBody {
    pub code: &'static str,
    pub message: &'static str,
}
