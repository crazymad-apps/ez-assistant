//! Runtime 可查询的只读业务快照和稳定状态。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    AttachmentId, ChildTaskId, InputId, ModelKey, RunId, RuntimeErrorInfo, SessionId, ToolCallId,
    WorkspaceId,
};

/// Runtime 对外可见的生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycle {
    /// Runtime 正常接受业务操作。
    Running,
    /// Runtime 已拒绝新工作，正在取消并结算活动 Run。
    ShuttingDown,
    /// Runtime 已完成受控关闭。
    Stopped,
}

/// Session 是否仍可接受业务变更。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    /// 可提交输入、重试、重新输入和切换模型。
    Active,
    /// 只允许查询，等待显式恢复。
    Archived,
}

/// Session 在产品中的稳定职责。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionRoleSnapshot {
    /// 普通工作会话。
    #[default]
    Standard,
    /// 用户统一协调入口。
    Controller,
}

/// 普通 Session 当前绑定的主控代理。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionProxySnapshot {
    pub controller_session_id: SessionId,
    pub changed_at_ms: i64,
}

/// 当前产品可使用的主控会话。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ControllerAvailabilitySnapshot {
    Available {
        session_id: SessionId,
    },
    #[default]
    Unavailable,
}

/// 当前 Session 正在执行的易失压缩操作。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionCompactionSnapshot {
    pub compaction_id: String,
    pub trigger: SessionCompactionTriggerSnapshot,
    pub source_generation: u64,
    pub started_at_ms: i64,
    pub cancellable: bool,
}

/// Session 压缩的发起来源。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCompactionTriggerSnapshot {
    Manual,
    Automatic {
        run_id: RunId,
        reason: SessionCompactionReasonSnapshot,
    },
}

/// 自动压缩的稳定触发原因。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionCompactionReasonSnapshot {
    ThresholdReached,
    ProviderOverflow,
}

/// 跨 Provider 稳定的推理强度 key。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortKey {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Agent 在一次用户输入中采用的行为变体。
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS,
)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum AgentVariant {
    /// 直接实施用户请求。
    #[default]
    Build,
    /// 先调查并形成实施计划；只可在 Agent 私有空间写入分析产物。
    Plan,
}

/// 一份权限文件所属的业务层级。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Global,
    Workspace,
    Session,
}

/// 显式重载时观察到的单个权限文件状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionFileStatus {
    Empty,
    Ready,
    Invalid,
    Unavailable,
}

/// 权限文件诊断的稳定分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PermissionDiagnosticCode {
    InvalidDocument,
    UnsupportedSchema,
    InvalidRule,
    UnsafePermissions,
    Unavailable,
}

/// 不回显文件正文或内部路径的权限诊断。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionDiagnostic {
    pub scope: PermissionScope,
    pub code: PermissionDiagnosticCode,
    pub message: String,
}

/// 一次 cohort reload 中某层权限文件的投影。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PermissionFileSummary {
    pub scope: PermissionScope,
    pub status: PermissionFileStatus,
}

/// Session 当前的审批交互方式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// 需要授权时询问用户。
    #[default]
    Ask,
    /// 按已配置规则自动决定，不发起临时询问。
    Auto,
}

/// Session 列表的生命周期过滤条件。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionListFilter {
    /// 只返回活动 Session。
    #[default]
    Active,
    /// 只返回归档 Session。
    Archived,
    /// 返回全部 Session。
    All,
}

/// Session 标题的来源，用于决定自动标题是否仍可被 Runtime 更新。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionTitleOrigin {
    #[default]
    Generated,
    User,
}

/// Session 标题模型调用的触发来源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionTitleGenerationTriggerSnapshot {
    Automatic,
    Manual,
}

/// 当前 Runtime 进程中一次正在进行的标题生成投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionTitleGenerationSnapshot {
    pub trigger: SessionTitleGenerationTriggerSnapshot,
    pub started_at_ms: i64,
}

/// Workspace 是否仍可供新 Session 选择。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycle {
    /// Workspace 正常登记，可绑定新 Session。
    Active,
    /// Workspace 已从正常选择列表移除，但历史绑定和目录均保留。
    Removed,
}

/// 一个 Workspace 的稳定业务投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct WorkspaceSummary {
    /// Runtime 分配的不透明 Workspace 标识。
    pub workspace_id: WorkspaceId,
    /// 用户可编辑的 Workspace 展示名称。
    #[serde(default)]
    pub label: String,
    /// Host canonicalize 后保存的用户工作目录。
    pub user_directory: String,
    /// 当前 Workspace 的有序附加目录；既有 Session 仍使用自己的冻结快照。
    #[serde(default)]
    pub additional_directories: Vec<String>,
    /// Runtime Home 中由 Host 管理的 Workspace 级 Agent 私有目录。
    pub agent_directory: String,
    /// Workspace 当前是否可供新 Session 选择。
    pub lifecycle: WorkspaceLifecycle,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub removed_at_ms: Option<i64>,
}

/// Attachment 的物理正文和稳定视图当前是否可用。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum AttachmentState {
    Ready,
    Unavailable,
}

/// 一个 Session Attachment 的客户端可见业务投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AttachmentSummary {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub media_type: Option<String>,
    pub agent_readable_path: String,
    pub state: AttachmentState,
    pub created_at_ms: i64,
}

/// Runtime 业务 Run 的活动态和终态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Runtime 已原子登记 Run，但 supervisor 尚未开始执行。
    Accepted,
    /// AgentExecution 正在运行。
    Running,
    /// 已收到取消请求，正在等待执行可靠收敛。
    Cancelling,
    /// Run 正常完成。
    Completed,
    /// Run 执行失败。
    Failed,
    /// Run 已取消并完成结算。
    Cancelled,
    /// Runtime 重启前没有可靠终结；不会自动恢复执行。
    Interrupted,
    /// Core 要求上层进行上下文压缩；正式 Runtime 通常在业务 Run 内部消费该交接，
    /// 该状态保留给不具备自动 continuation 的宿主及历史兼容读取。
    CompactionRequired,
}

impl RunStatus {
    /// 判断状态是否已经不可再次改变。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::CompactionRequired
        )
    }
}

/// 父 Run 管理的子任务生命周期。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ChildTaskStatus {
    /// 父委派已可靠创建关系，尚未启动子执行。
    Accepted,
    /// 子任务正文已经写入，子执行正在运行。
    Running,
    /// 子任务正常形成最终 Assistant Message。
    Completed,
    /// 子任务因模型、预算、Guardrail、存储或超时失败。
    Failed,
    /// 子任务收到取消并完成协作式收敛。
    Cancelled,
    /// Runtime 重启前没有可靠终态，不自动续跑。
    Interrupted,
}

impl ChildTaskStatus {
    /// 判断子任务是否已经不可再次改变。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// 一个单层子任务的可重建业务快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ChildTaskSnapshot {
    pub child_task_id: ChildTaskId,
    pub session_id: SessionId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub status: ChildTaskStatus,
    pub variant: AgentVariant,
    pub cancel_requested: bool,
    pub final_text: String,
    pub error: Option<RuntimeErrorInfo>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

/// 工具调用在 Run 观察投影中的状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    /// 模型已提出调用，但调用尚未开始执行。
    Proposed,
    /// 调用已通过授权并开始执行。
    Running,
    /// 调用已成功完成。
    Completed,
    /// 调用因拒绝、输入或执行错误而失败。
    Failed,
}

/// 工具流式输出的应用层通道。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputChannel {
    /// 标准输出。
    Stdout,
    /// 标准错误输出。
    Stderr,
}

/// Runtime Guardrail 的稳定检测类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum GuardrailKind {
    RepeatedInvocation,
    ConsecutiveFailures,
}

/// Guardrail 达到阈值后的产品层行为。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum GuardrailMode {
    Observe,
    Enforce,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// 只允许当前待执行 Tool Call。
    AllowOnce,
    /// 写入当前 Session 权限文件后允许当前调用。
    AllowSession,
    /// 写入已绑定 Workspace 权限文件后允许当前调用。
    AllowWorkspace,
    /// 拒绝当前 Tool Call，不产生持久规则。
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    /// 可以由一个客户端原子取得决策权。
    Pending,
    /// 一个决策正在完成持久化及唤醒操作。
    Resolving,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolApprovalSubject {
    /// 不带文件或进程专属授权事实的工具调用。
    General { tool_name: String },
    /// 创建单层子任务的委派调用；目标只保留受限展示摘要。
    Delegation {
        tool_name: String,
        title: String,
        task_summary: String,
    },
    /// 已解析为绝对路径的结构化文件操作。
    File {
        tool_name: String,
        operation: String,
        path: String,
    },
    /// 一次工具调用中已分别解析的多条结构化文件路径。
    Files {
        tool_name: String,
        operation: String,
        paths: Vec<String>,
    },
    /// 已解析完整命令和工作目录的 Shell 操作。
    Shell {
        tool_name: String,
        command: String,
        working_directory: String,
        timeout_ms: u64,
        process_mode: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApprovalSnapshot {
    /// Runtime 内存中一次审批的不透明标识。
    pub approval_id: crate::ApprovalId,
    /// 审批所属 Session。
    pub session_id: SessionId,
    /// 审批所属 Run。
    pub run_id: RunId,
    /// 子任务内部调用的审批归属；父 Run 自身调用保持为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_task_id: Option<crate::ChildTaskId>,
    /// 等待执行的 Tool Call。
    pub call_id: ToolCallId,
    /// 当前 Run 冻结的 Agent 变体。
    pub variant: AgentVariant,
    /// 当前 Run 冻结的审批方式；显式 Ask 规则也可能在 Auto 中产生审批。
    pub approval_mode: ApprovalMode,
    /// 用于客户端展示的已解析调用事实。
    pub subject: ToolApprovalSubject,
    /// 服务端按当前作用域计算出的合法决定。
    pub available_decisions: Vec<ApprovalDecision>,
    /// 持久允许将写入的精确匹配语义预览；不支持持久授权的多路径调用仅作展示。
    pub exact_rule_preview: ToolApprovalSubject,
    /// 当前是否仍可直接取得决策权。
    pub status: ApprovalStatus,
    /// Runtime 创建审批的 Unix 毫秒时间。
    pub created_at_ms: i64,
}

/// 一个 Session 的稳定摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionSummary {
    /// Session 的不透明标识。
    pub session_id: SessionId,
    /// 创建 Session 时确定的展示标题。
    pub title: String,
    /// Session 后续 Run 当前使用的用户模型 key。
    pub model_key: ModelKey,
    /// 后续 Run 使用的显式强度；空值表示使用模型默认开启档位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning_effort: Option<ReasoningEffortKey>,
    /// Session 当前是活动还是归档状态。
    pub lifecycle: SessionLifecycle,
    /// Session 的持久产品角色；旧快照安全缺省为普通会话。
    #[serde(default)]
    pub role: SessionRoleSnapshot,
    /// 普通 Session 当前代理归属；主控自身始终为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proxy: Option<SessionProxySnapshot>,
    /// Controller 的 PC 输出当前附加托管目标；普通 Session 始终为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pc_output_hosting: Option<PcOutputHostingSnapshot>,
    /// 只存在于当前 Runtime 进程内；重启后必然为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_compaction: Option<SessionCompactionSnapshot>,
    /// UI 当前选中的 Agent 变体；具体 Run 仍以提交 Input 携带的值为准。
    #[serde(default)]
    pub current_variant: AgentVariant,
    /// 后续 Run 捕获的当前审批模式。
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    /// 创建 Session 时冻结的 Workspace 绑定；`None` 表示普通未绑定会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// 当前活动 Run；Session 空闲时为 `None`。
    pub active_run_id: Option<RunId>,
    /// 规范 Conversation 中已经完整提交的消息数量。
    pub message_count: u64,
    /// 尚未进入规范 Conversation 的持久化输入数量。
    pub queued_input_count: u64,
    /// 重启恢复后队列是否等待用户显式继续。
    pub resume_required: bool,
    /// 老记录无法可靠恢复时保持为空。
    #[serde(default)]
    pub created_at_ms: Option<i64>,
    /// 最近一次 Run 可靠终结的时间；尚无 Run 时等于创建时间。
    /// 标题、固定状态、模式等元数据变更不得推进该时间。
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
    #[serde(default)]
    pub archived_at_ms: Option<i64>,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub title_origin: SessionTitleOrigin,
    #[serde(default)]
    pub pending_approval_count: u64,
    #[serde(default)]
    pub active_child_count: u64,
    #[serde(default)]
    pub active_run_status: Option<RunStatus>,
}

/// Controller 当前稳定的 PC 输出托管目标。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PcOutputHostingSnapshot {
    pub device_id: crate::DeviceId,
    pub device_name: String,
}

/// Run 中一个工具调用的当前观察快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ToolActivitySnapshot {
    /// 所属 Run step；旧快照缺失时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step: Option<u32>,
    /// 工具调用的不透明标识。
    pub call_id: ToolCallId,
    /// 模型可见的工具名称。
    pub tool_name: String,
    /// 当前工具活动状态。
    pub status: ToolActivityStatus,
    /// 截至快照时观察到的标准输出；事件丢失时可能不完整。
    pub stdout: String,
    /// 截至快照时观察到的标准错误输出；事件丢失时可能不完整。
    pub stderr: String,
}

/// 一次完整模型请求由 Provider 最终确认的 token 用量。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TokenUsageSnapshot {
    /// 本次请求消耗的输入 token。
    pub input_tokens: u64,
    /// 本次响应产生的输出 token。
    pub output_tokens: u64,
    /// Provider 报告的总 token；不在应用层重新计算。
    pub total_tokens: u64,
    /// 输入 token 中命中缓存的数量；Provider 未提供时为 `None`。
    pub cached_input_tokens: Option<u64>,
}

/// 一个 Runtime Run 的当前只读快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RunSnapshot {
    /// Run 的不透明标识。
    pub run_id: RunId,
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 本次 Run 所属的持久化输入。
    pub input_id: InputId,
    /// 同一输入的执行尝试序号，从 1 开始。
    pub attempt: u32,
    /// Run 被接受的时间；旧记录无法恢复时为空。
    #[serde(default)]
    pub created_at_ms: Option<i64>,
    /// Run 可靠终结的时间；活动 Run 或旧记录无法恢复时为空。
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    /// 当前活动态或终态。
    pub status: RunStatus,
    /// 本次 Run 继承的 Input Agent 变体。
    #[serde(default)]
    pub variant: AgentVariant,
    /// 本次 Run 创建时捕获的 Session 审批模式。
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    /// Run 启动时冻结的实际强度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning_effort: Option<ReasoningEffortKey>,
    /// 是否已经接受过取消请求。
    pub cancel_requested: bool,
    /// 当前流式观察内容所属的 Run step；尚未开始或旧快照缺失时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_step: Option<u32>,
    /// 截至快照时观察到的 reasoning；运行中可能因事件丢失而不完整。
    pub reasoning: String,
    /// 截至快照时观察到的正文；正常完成时由最终 AssistantMessage 校准。
    pub text: String,
    /// 当前已观察到的工具调用。
    pub tools: Vec<ToolActivitySnapshot>,
    /// Run 失败时可安全跨层展示的错误；其他状态为 `None`。
    pub error: Option<RuntimeErrorInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeErrorCode;

    #[test]
    fn run_status_serialization_and_terminal_semantics_are_stable() {
        let cases = [
            (RunStatus::Accepted, "accepted", false),
            (RunStatus::Running, "running", false),
            (RunStatus::Cancelling, "cancelling", false),
            (RunStatus::Completed, "completed", true),
            (RunStatus::Failed, "failed", true),
            (RunStatus::Cancelled, "cancelled", true),
            (RunStatus::Interrupted, "interrupted", true),
            (RunStatus::CompactionRequired, "compaction_required", true),
        ];

        for (status, wire, terminal) in cases {
            assert_eq!(
                serde_json::to_string(&status).expect("serialize status"),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<RunStatus>(&format!("\"{wire}\""))
                    .expect("deserialize status"),
                status
            );
            assert_eq!(status.is_terminal(), terminal);
        }
    }

    #[test]
    fn child_task_status_serialization_and_terminal_semantics_are_stable() {
        let cases = [
            (ChildTaskStatus::Accepted, "accepted", false),
            (ChildTaskStatus::Running, "running", false),
            (ChildTaskStatus::Completed, "completed", true),
            (ChildTaskStatus::Failed, "failed", true),
            (ChildTaskStatus::Cancelled, "cancelled", true),
            (ChildTaskStatus::Interrupted, "interrupted", true),
        ];
        for (status, wire, terminal) in cases {
            assert_eq!(
                serde_json::to_string(&status).expect("serialize child status"),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<ChildTaskStatus>(&format!("\"{wire}\""))
                    .expect("deserialize child status"),
                status
            );
            assert_eq!(status.is_terminal(), terminal);
        }
    }

    #[test]
    fn supporting_status_enums_use_stable_snake_case_values() {
        let lifecycles = [
            (RuntimeLifecycle::Running, "running"),
            (RuntimeLifecycle::ShuttingDown, "shutting_down"),
            (RuntimeLifecycle::Stopped, "stopped"),
        ];
        for (value, wire) in lifecycles {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize lifecycle"),
                format!("\"{wire}\"")
            );
        }

        let session_lifecycles = [
            (SessionLifecycle::Active, "active"),
            (SessionLifecycle::Archived, "archived"),
        ];
        for (value, wire) in session_lifecycles {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize session lifecycle"),
                format!("\"{wire}\"")
            );
        }
        assert_eq!(SessionListFilter::default(), SessionListFilter::Active);

        let activities = [
            (ToolActivityStatus::Proposed, "proposed"),
            (ToolActivityStatus::Running, "running"),
            (ToolActivityStatus::Completed, "completed"),
            (ToolActivityStatus::Failed, "failed"),
        ];
        for (value, wire) in activities {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize tool status"),
                format!("\"{wire}\"")
            );
        }

        assert_eq!(
            serde_json::to_string(&ToolOutputChannel::Stdout).expect("serialize stdout"),
            "\"stdout\""
        );
        assert_eq!(
            serde_json::to_string(&ToolOutputChannel::Stderr).expect("serialize stderr"),
            "\"stderr\""
        );
    }

    #[test]
    fn run_snapshot_round_trips_without_internal_runtime_state() {
        let snapshot = RunSnapshot {
            run_id: RunId::new("run-1").expect("run id"),
            session_id: SessionId::new("session-1").expect("session id"),
            input_id: InputId::new("input-1").expect("input id"),
            attempt: 1,
            created_at_ms: Some(1),
            finished_at_ms: Some(2),
            status: RunStatus::Failed,
            variant: AgentVariant::Plan,
            approval_mode: ApprovalMode::Auto,
            reasoning_effort: None,
            cancel_requested: false,
            active_step: Some(1),
            reasoning: "checked".to_owned(),
            text: "partial".to_owned(),
            tools: vec![ToolActivitySnapshot {
                step: Some(1),
                call_id: ToolCallId::new("call-1").expect("call id"),
                tool_name: "echo_text".to_owned(),
                status: ToolActivityStatus::Completed,
                stdout: "hello".to_owned(),
                stderr: String::new(),
            }],
            error: Some(RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "run failed",
            )),
        };

        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: RunSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn legacy_session_and_run_snapshots_default_to_build_and_ask() {
        let session = serde_json::json!({
            "session_id": "session-1",
            "title": "Legacy",
            "model_key": "model-1",
            "lifecycle": "active",
            "active_run_id": null,
            "message_count": 0,
            "queued_input_count": 0,
            "resume_required": false
        });
        let session: SessionSummary = serde_json::from_value(session).expect("legacy session");
        assert_eq!(session.current_variant, AgentVariant::Build);
        assert_eq!(session.approval_mode, ApprovalMode::Ask);
        assert_eq!(session.role, SessionRoleSnapshot::Standard);
        assert_eq!(session.proxy, None);
        assert_eq!(session.pc_output_hosting, None);

        let run = serde_json::json!({
            "run_id": "run-1",
            "session_id": "session-1",
            "input_id": "input-1",
            "attempt": 1,
            "status": "completed",
            "cancel_requested": false,
            "reasoning": "",
            "text": "done",
            "tools": [],
            "error": null
        });
        let run: RunSnapshot = serde_json::from_value(run).expect("legacy run");
        assert_eq!(run.variant, AgentVariant::Build);
        assert_eq!(run.approval_mode, ApprovalMode::Ask);
        assert_eq!(run.created_at_ms, None);
        assert_eq!(run.finished_at_ms, None);
    }

    #[test]
    fn approval_snapshot_preserves_subject_and_available_decisions() {
        let snapshot = ApprovalSnapshot {
            approval_id: crate::ApprovalId::new("approval-1").expect("approval id"),
            session_id: SessionId::new("session-1").expect("session id"),
            run_id: RunId::new("run-1").expect("run id"),
            child_task_id: None,
            call_id: ToolCallId::new("call-1").expect("call id"),
            variant: AgentVariant::Plan,
            approval_mode: ApprovalMode::Ask,
            subject: ToolApprovalSubject::Shell {
                tool_name: "shell".to_owned(),
                command: "git status".to_owned(),
                working_directory: "/workspace".to_owned(),
                timeout_ms: 30_000,
                process_mode: "managed".to_owned(),
            },
            available_decisions: vec![
                ApprovalDecision::AllowOnce,
                ApprovalDecision::AllowSession,
                ApprovalDecision::Deny,
            ],
            exact_rule_preview: ToolApprovalSubject::Shell {
                tool_name: "shell".to_owned(),
                command: "git status".to_owned(),
                working_directory: "/workspace".to_owned(),
                timeout_ms: 30_000,
                process_mode: "managed".to_owned(),
            },
            status: ApprovalStatus::Pending,
            created_at_ms: 10,
        };

        let value = serde_json::to_value(&snapshot).expect("serialize approval");
        assert_eq!(value["subject"]["type"], "shell");
        assert_eq!(value["available_decisions"][1], "allow_session");
        assert!(value.get("child_task_id").is_none());
        assert_eq!(
            serde_json::from_value::<ApprovalSnapshot>(value).expect("deserialize approval"),
            snapshot
        );
    }

    #[test]
    fn delegation_approval_subject_round_trips_as_an_additive_variant() {
        let subject = ToolApprovalSubject::Delegation {
            tool_name: "delegate_task".to_owned(),
            title: "Inspect storage".to_owned(),
            task_summary: "Review the storage boundary and report risks.".to_owned(),
        };

        let value = serde_json::to_value(&subject).expect("serialize delegation subject");
        assert_eq!(value["type"], "delegation");
        assert_eq!(value["tool_name"], "delegate_task");
        assert_eq!(value["title"], "Inspect storage");
        assert_eq!(
            serde_json::from_value::<ToolApprovalSubject>(value)
                .expect("deserialize delegation subject"),
            subject
        );
    }

    #[test]
    fn multi_file_approval_subject_preserves_each_resolved_path() {
        let subject = ToolApprovalSubject::Files {
            tool_name: "inspect_images".to_owned(),
            operation: "read".to_owned(),
            paths: vec!["/workspace/a.png".to_owned(), "/tmp/b.png".to_owned()],
        };

        let value = serde_json::to_value(&subject).expect("serialize multi-file subject");
        assert_eq!(value["type"], "files");
        assert_eq!(value["operation"], "read");
        assert_eq!(
            value["paths"],
            serde_json::json!(["/workspace/a.png", "/tmp/b.png"])
        );
        assert_eq!(
            serde_json::from_value::<ToolApprovalSubject>(value)
                .expect("deserialize multi-file subject"),
            subject
        );
    }
}
