//! Runtime 持久化端口及跨基础设施边界的存储 DTO。
//!
//! 本模块使用业务操作表达 Runtime 对存储的需求，不暴露 SQL、路径、文件 offset 或
//! SQLite 实体。正式本地实现由 Runtime Host 装配；Runtime crate 不依赖具体数据库。

use std::{error::Error, fmt, future::Future, pin::Pin};

use agent_core::ExchangeReceipt;
use agent_model::SystemPromptSnapshot;
use agent_types::{
    AssistantMessage, ConversationMessage, ConversationSnapshot, MessageId, ToolMessage,
    UserMessage,
};
use assistant_protocol::{
    AgentVariant, ApprovalMode, AttachmentId, ChildTaskId, ChildTaskStatus, IdempotencyKey,
    InputId, ModelKey, RunId, RunStatus, RuntimeErrorInfo, SessionId, ToolCallId, WorkspaceId,
};

use crate::SessionExecutionEnvironment;

mod volatile;

pub(crate) use volatile::VolatileRuntimeStore;

/// Runtime Store 异步操作的统一 Future。
pub type StoreFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, StoreError>> + Send + 'a>>;

/// 调用方可以据此选择重试、隔离 Session 或停止 Runtime 的稳定错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreErrorKind {
    /// 存储 worker 已关闭、失联或无法提供服务。
    Unavailable,
    /// 持久化内容无法解析或破坏领域不变量。
    InvalidData,
    /// 当前持久化状态与命令前置条件冲突。
    Conflict,
    /// 调用方提供的存储命令不满足边界约束。
    InvalidInput,
    /// Store 管理的外部资源当前不存在或不可访问。
    ResourceUnavailable,
    /// 本地 I/O 或数据库操作失败。
    Internal,
}

/// Runtime Store 失败；Display 只包含安全稳定信息，具体 source 留在进程内诊断。
#[derive(Debug)]
pub struct StoreError {
    kind: StoreErrorKind,
    message: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl StoreError {
    /// 创建不带底层 source 的安全存储错误。
    pub fn new(kind: StoreErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            source: None,
        }
    }

    /// 创建带进程内诊断 source 的安全存储错误。
    pub fn with_source(
        kind: StoreErrorKind,
        message: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message,
            source: Some(Box::new(source)),
        }
    }

    /// 返回稳定错误分类。
    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    /// 返回不包含路径、正文或数据库细节的安全消息。
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Session 的持久化生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredSessionLifecycle {
    /// 可以接受业务变更的活动 Session。
    Active,
    /// 只允许查询、等待显式恢复的归档 Session。
    Archived,
}

/// 启动恢复后正文是否可以安全加载。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredConversationState {
    /// 当前 generation 可读取且没有未决的一致性故障。
    Available,
    /// 该 Session 的正文存在无法自动判定的持久化状态。
    Unavailable,
}

/// Workspace 的持久化生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredWorkspaceLifecycle {
    Active,
    Removed,
}

/// Host Store 恢复或写入完成的 Workspace 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredWorkspace {
    pub workspace_id: WorkspaceId,
    pub user_directory: String,
    pub agent_directory: String,
    pub lifecycle: StoredWorkspaceLifecycle,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub removed_at_ms: Option<i64>,
}

/// Attachment 的正文及 Session 稳定视图是否可供 Agent 读取。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredAttachmentState {
    Ready,
    Unavailable,
}

/// Host Store 恢复或写入完成的 Attachment 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAttachment {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    /// 由原始文件名和文件字节共同计算的 Blob 身份摘要。
    pub blob_hash: String,
    pub size_bytes: u64,
    pub agent_readable_path: String,
    pub state: StoredAttachmentState,
    pub created_at_ms: i64,
}

/// Host 已流式接收并校验、等待 Store 原子完成的 Attachment 上传。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAttachmentUpload {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    pub staging_path: String,
    /// 由原始文件名和文件字节共同计算的 Blob 身份摘要。
    pub blob_hash: String,
    pub size_bytes: u64,
    pub created_at_ms: i64,
}

/// Runtime 请求 Store 登记或按 canonical path 恢复 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewWorkspaceRegistration {
    pub workspace_id: WorkspaceId,
    pub requested_directory: String,
    pub changed_at_ms: i64,
}

/// Runtime 请求 Store 假删 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRemoval {
    pub workspace_id: WorkspaceId,
    pub changed_at_ms: i64,
}

/// 创建持久化 Session 所需的完整冻结事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredSession {
    pub session_id: SessionId,
    pub title: String,
    pub model_key: ModelKey,
    pub system_prompt: SystemPromptSnapshot,
    pub environment: SessionExecutionEnvironment,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub created_at_ms: i64,
}

/// 从存储恢复或创建完成的 Session 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSession {
    pub session_id: SessionId,
    pub title: String,
    pub model_key: ModelKey,
    pub system_prompt: SystemPromptSnapshot,
    pub environment: SessionExecutionEnvironment,
    pub lifecycle: StoredSessionLifecycle,
    pub current_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub body_generation: u64,
    pub message_count: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub archived_at_ms: Option<i64>,
    pub conversation_state: StoredConversationState,
}

/// 队列执行器领取一次 Run 时提交的 User Message 与结构化关联。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMessageCommit {
    pub operation_id: String,
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    /// queued 输入首次开始时为 Some；已提交输入的新 attempt 不重复追加消息。
    pub message: Option<UserMessage>,
    pub created_at_ms: i64,
}

/// 工具副作用发生前必须可靠保存的完整 Assistant Tool Call 批次。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolExchange {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub assistant: AssistantMessage,
    pub created_at_ms: i64,
}

/// 工具结果齐备后，把 pending 批次整体转入规范 Conversation 的命令。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedToolExchange {
    pub operation_id: String,
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub results: Vec<ToolMessage>,
    pub completed_at_ms: i64,
}

/// Core 已获授权、即将进入工具副作用前的可靠 started 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolExecutionStart {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub started_at_ms: i64,
}

/// Input 是否已经进入规范 Conversation。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredInputState {
    /// 正文暂存在结构化队列中，尚可取消。
    Queued,
    /// User Message 已提交到规范 Conversation，不再属于可取消队列。
    Committed,
}

/// Runtime 从 Store 恢复的 Input 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredInput {
    pub queue_order: u64,
    pub input_id: InputId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub user_message_id: MessageId,
    pub state: StoredInputState,
    pub queued_message: Option<UserMessage>,
    pub accepted_at_ms: i64,
}

/// 原子接受 Input 及其首次 Run 所需的完整事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredInput {
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub message: UserMessage,
    pub accepted_at_ms: i64,
}

/// Store 接受结果；幂等命中时返回首次持久化事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedInput {
    pub input: StoredInput,
    pub run: StoredRun,
    pub is_duplicate: bool,
}

/// 从失败或中断 Run 创建下一次执行尝试的命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredRunAttempt {
    pub run_id: RunId,
    pub source_run_id: RunId,
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
    pub created_at_ms: i64,
}

/// 一次 Run 的可靠终态以及尚未写入正文的完整消息批次。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRunSettlement {
    pub operation_id: String,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub messages: Vec<ConversationMessage>,
    pub finished_at_ms: i64,
}

/// Runtime 启动时恢复的 Run 结构化投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredRun {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub input_id: InputId,
    pub attempt: u32,
    pub status: RunStatus,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub message_ids: Vec<MessageId>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

/// 创建 accepted 子任务关系及空独立正文所需的冻结事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredChildTask {
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub system_prompt: SystemPromptSnapshot,
    pub agent_variant: AgentVariant,
    pub created_at_ms: i64,
}

/// Runtime 从 Store 恢复或创建完成的子任务结构化投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredChildTask {
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub system_prompt: SystemPromptSnapshot,
    pub agent_variant: AgentVariant,
    pub status: ChildTaskStatus,
    pub cancel_requested: bool,
    pub body_generation: u64,
    pub message_count: u64,
    pub final_message_id: Option<MessageId>,
    pub error: Option<RuntimeErrorInfo>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    /// 子任务独立正文是否可以安全读取；不额外持久化到 SQLite。
    pub conversation_state: StoredConversationState,
}

/// 把子任务初始 User Message 可靠写入独立正文并切到 running。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildTaskStart {
    pub operation_id: String,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub message: UserMessage,
    pub started_at_ms: i64,
}

/// 子任务工具副作用前必须保存的完整 Assistant Tool Call 批次。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingChildToolExchange {
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub assistant: AssistantMessage,
    pub created_at_ms: i64,
}

/// 子任务 Core 已获授权、即将进入工具副作用前的可靠 started 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildToolExecutionStart {
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub call_id: ToolCallId,
    pub started_at_ms: i64,
}

/// 子任务工具结果齐备后的可靠完整批次提交。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedChildToolExchange {
    pub operation_id: String,
    pub receipt: ExchangeReceipt,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub results: Vec<ToolMessage>,
    pub completed_at_ms: i64,
}

/// 子任务最终状态及尚未写入独立正文的完整消息批次。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredChildTaskSettlement {
    pub operation_id: String,
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub status: ChildTaskStatus,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub messages: Vec<ConversationMessage>,
    pub final_message_id: Option<MessageId>,
    pub finished_at_ms: i64,
}

/// Runtime 启动时一次性取得的结构化恢复结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveredRuntime {
    pub workspaces: Vec<StoredWorkspace>,
    pub attachments: Vec<StoredAttachment>,
    pub sessions: Vec<StoredSession>,
    pub inputs: Vec<StoredInput>,
    pub runs: Vec<StoredRun>,
    pub child_tasks: Vec<StoredChildTask>,
}

/// 原子切换 Session 归档状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveChange {
    pub session_id: SessionId,
    pub archived: bool,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 后续 Run 使用的模型 key。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChange {
    pub session_id: SessionId,
    pub model_key: ModelKey,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 当前 Agent 变体。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantChange {
    pub session_id: SessionId,
    pub variant: AgentVariant,
    pub changed_at_ms: i64,
}

/// 原子切换 Session 当前审批模式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalModeChange {
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
    pub changed_at_ms: i64,
}

/// 历史重新输入所需的完整新正文和结构化关联。
#[derive(Clone, Debug, PartialEq)]
pub struct ConversationRewrite {
    pub session_id: SessionId,
    pub target_user_message_id: MessageId,
    pub conversation: ConversationSnapshot,
    pub input: NewStoredInput,
    pub changed_at_ms: i64,
}

/// 自动压缩要原子替换的正文所有者；Run 与 child 都必须仍处于活动执行期。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextReplacementTarget {
    Run {
        session_id: SessionId,
        run_id: RunId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
}

/// 使用新 generation 替换当前有效 Conversation，不改写历史 Input/Run/child 关系。
#[derive(Clone, Debug, PartialEq)]
pub struct ContextReplacement {
    pub target: ContextReplacementTarget,
    pub conversation: ConversationSnapshot,
    pub changed_at_ms: i64,
}

/// generation 切换成功后创建的新 Input 与首次 Run。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteResult {
    pub input: StoredInput,
    pub run: StoredRun,
}

/// Assistant Runtime 使用的持久化能力端口。
///
/// M2 已覆盖恢复、Session、Input 准入/取消、Run attempt、领取起始和终态结算；后续里程碑继续按
/// 已确认业务原子操作扩展工具交换和会话生命周期，禁止退化为通用 SQL 或键值接口。
pub trait RuntimeStore: Send + Sync {
    /// 恢复未完成提交并加载 Runtime 的结构化启动投影。
    fn load_runtime(&self) -> StoreFuture<'_, RecoveredRuntime>;

    /// 按 canonical path 幂等登记或恢复 Workspace。
    fn register_workspace(
        &self,
        registration: NewWorkspaceRegistration,
    ) -> StoreFuture<'_, StoredWorkspace>;

    /// 假删 Workspace，不删除任何目录或历史绑定。
    fn remove_workspace(&self, removal: WorkspaceRemoval) -> StoreFuture<'_, StoredWorkspace>;

    /// 完成已流式接收到 staging 的上传；同 Session、同 Blob Hash 返回首次结果。
    fn upload_attachment(&self, upload: NewAttachmentUpload) -> StoreFuture<'_, StoredAttachment>;

    /// 创建 Session 稳定事实及其空 Conversation。
    fn create_session(&self, session: NewStoredSession) -> StoreFuture<'_, StoredSession>;

    /// 创建 accepted 子任务关系及其空的 generation 1 正文。
    fn create_child_task(&self, task: NewStoredChildTask) -> StoreFuture<'_, StoredChildTask>;

    /// 写入初始 User Message，并把 accepted 子任务可靠切到 running。
    fn start_child_task(&self, start: ChildTaskStart) -> StoreFuture<'_, ()>;

    /// 在任何子任务工具副作用前保存完整 Tool Call 批次。
    fn begin_child_tool_exchange(&self, pending: PendingChildToolExchange) -> StoreFuture<'_, ()>;

    /// 在子任务 Tool SPI 产生副作用前写入 started 标记。
    fn mark_child_tool_execution_started(
        &self,
        start: ChildToolExecutionStart,
    ) -> StoreFuture<'_, ()>;

    /// 提交子任务完整工具结果并清除对应 pending 事实。
    fn complete_child_tool_exchange(
        &self,
        completed: CompletedChildToolExchange,
    ) -> StoreFuture<'_, ()>;

    /// 可靠写入最终消息并结算子任务终态。
    fn settle_child_task(&self, settlement: StoredChildTaskSettlement) -> StoreFuture<'_, ()>;

    /// 在发布活动 child 取消令牌前可靠记录取消意图；终态任务幂等返回原投影。
    fn request_child_task_cancellation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, StoredChildTask>;

    /// 加载子任务独立的完整规范 Conversation，同时核对 Session 所有权。
    fn load_child_conversation(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StoreFuture<'_, ConversationSnapshot>;

    /// 可靠切换父 Run 或 child 的压缩后有效正文 generation。
    fn replace_context(&self, replacement: ContextReplacement) -> StoreFuture<'_, ()>;

    /// 原子创建 Input 与首次 Accepted Run，或返回同 Session 幂等 key 的首次结果。
    fn accept_input(&self, input: NewStoredInput) -> StoreFuture<'_, AcceptedInput>;

    /// 删除尚未进入规范 Conversation 的排队 Input 及其 Run。
    fn cancel_queued_input(
        &self,
        session_id: &SessionId,
        input_id: &InputId,
    ) -> StoreFuture<'_, ()>;

    /// 为最新的 Failed/Interrupted Run 创建递增 attempt。
    fn create_run_attempt(&self, attempt: NewStoredRunAttempt) -> StoreFuture<'_, StoredRun>;

    /// 可靠写入 User Message，并将对应 Run 从 accepted 转为 running。
    fn commit_user_message(&self, commit: UserMessageCommit) -> StoreFuture<'_, ()>;

    /// 在任何工具副作用前保存完整 Tool Call 批次并返回确认。
    fn begin_tool_exchange(&self, pending: PendingToolExchange) -> StoreFuture<'_, ()>;

    /// 在 Tool SPI 产生任何外部副作用前写入临时 started 标记。
    fn mark_tool_execution_started(&self, start: ToolExecutionStart) -> StoreFuture<'_, ()>;

    /// 保存完整结果、整批提交正文并清除对应 pending 事实。
    fn complete_tool_exchange(&self, completed: CompletedToolExchange) -> StoreFuture<'_, ()>;

    /// 可靠写入本 Run 尚未提交的完整消息，并同时结算 Run 终态。
    fn settle_run(&self, settlement: StoredRunSettlement) -> StoreFuture<'_, ()>;

    /// 按当前权威 generation 加载并校验完整规范 Conversation。
    fn load_conversation(&self, session_id: &SessionId) -> StoreFuture<'_, ConversationSnapshot>;

    /// 原子切换 Session 归档状态；正文和运行历史保持不变。
    fn set_session_archive(&self, change: ArchiveChange) -> StoreFuture<'_, ()>;

    /// 原子切换 Session 后续 Run 使用的模型 key。
    fn set_session_model(&self, change: ModelChange) -> StoreFuture<'_, ()>;

    /// 原子切换 Session 当前 Agent 变体。
    fn set_session_variant(&self, change: VariantChange) -> StoreFuture<'_, ()>;

    /// 原子切换 Session 当前审批模式。
    fn set_session_approval_mode(&self, change: ApprovalModeChange) -> StoreFuture<'_, ()>;

    /// 原子切换正文 generation、销毁目标及尾段关联，并创建新的 committed Input/Run。
    fn rewrite_from_user(&self, rewrite: ConversationRewrite) -> StoreFuture<'_, RewriteResult>;

    /// 停止接收新命令，flush 已接受操作并等待基础设施 worker 退出。
    fn shutdown(&self) -> StoreFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixtureSource;

    impl fmt::Display for FixtureSource {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("private path /tmp/fixture")
        }
    }

    impl Error for FixtureSource {}

    #[test]
    fn display_is_safe_while_source_remains_available_in_process() {
        let error = StoreError::with_source(
            StoreErrorKind::Internal,
            "runtime storage operation failed",
            FixtureSource,
        );

        assert_eq!(error.to_string(), "runtime storage operation failed");
        assert_eq!(error.kind(), StoreErrorKind::Internal);
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("private path /tmp/fixture")
        );
    }
}
