//! Desktop 使用的分页、组合查询与安全展示投影。
//!
//! 这些类型描述产品领域事实，不包含页面展开状态、像素布局、凭据或 Runtime 私有存储结构。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ApprovalId, ApprovalSnapshot, AttachmentId, AttachmentSummary, ChildTaskId, ChildTaskSnapshot,
    ConfigurationStatus, GoalId, InputId, MessageId, ModelConfiguration, ModelKey, PartId,
    ReasoningEffortKey, RunId, RunSnapshot, RunStatus, RuntimeErrorInfo, RuntimeLifecycle,
    SessionId, SessionLifecycle, SessionSummary, TodoItemId, TokenUsageSnapshot,
    ToolActivityStatus, ToolCallId, WorkspaceSummary,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReasoningEffortOptionSnapshot {
    pub key: ReasoningEffortKey,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ImageHandlingMode {
    Native,
    Tool,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ComposerCapabilitiesSnapshot {
    /// 当前 Session 历史模型键仍能解析为可用配置时返回该键；否则前端必须视为未选择模型。
    #[serde(default)]
    pub selected_model_key: Option<ModelKey>,
    pub reasoning_effort_options: Vec<ReasoningEffortOptionSnapshot>,
    pub image_handling: ImageHandlingMode,
    /// 当前冻结模型是否支持 Goal 所需的 Tool Call。
    #[serde(default)]
    pub goal_supported: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum TodoItemStatusSnapshot {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct WorkPlanItemSnapshot {
    pub id: TodoItemId,
    pub text: String,
    pub status: TodoItemStatusSnapshot,
}

/// Session 当前唯一工作计划；它与 Goal 是否自动续跑正交。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct WorkPlanSnapshot {
    pub revision: u64,
    pub objective: String,
    pub items: Vec<WorkPlanItemSnapshot>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum GoalStateSnapshot {
    Running,
    Paused,
    Completed,
}

/// Goal 暂停原因；Blocked summary 是 Agent 提交的安全、有限摘要，不包含 objective 正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoalPauseReasonSnapshot {
    Blocked { summary: String },
    UserStopped,
    RunLimitReached,
    TokenLimitReached,
    ConsecutiveFailures,
    RecoveryRequired,
    Forked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GoalBudgetSnapshot {
    pub max_runs: u32,
    pub max_total_tokens: u64,
    pub max_consecutive_failures: u32,
    pub used_runs: u32,
    pub used_total_tokens: u64,
    pub consecutive_failures: u32,
    /// false 表示至少一次 Provider 未报告完整 usage；Runtime 不对缺失 token 做猜测。
    pub usage_complete: bool,
}

/// Desktop 展示所需的 Goal 最小投影；不包含恢复 payload、注入正文或内容哈希。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GoalSnapshot {
    pub goal_id: GoalId,
    pub objective_message_id: MessageId,
    pub objective_preview: String,
    pub attachment_count: u32,
    /// Goal 续跑沿用的服务身份；展示不查询当前目录，旧目标缺省为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_server_key: Option<crate::McpServerKey>,
    pub state: GoalStateSnapshot,
    pub pause_reason: Option<GoalPauseReasonSnapshot>,
    pub generation: u64,
    pub turn: u32,
    pub budget: GoalBudgetSnapshot,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

/// 带有 Runtime 观察水位的权威快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ObservedSnapshot<T> {
    /// 快照完成时已经纳入投影的最后一个事件序号。
    pub observed_sequence: u64,
    /// 领域投影。
    pub value: T,
}

/// 当前 Desktop 可以依赖的产品能力。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApplicationCapabilities {
    pub conversation_paging: bool,
    /// 是否支持跨会话正文检索和按消息定位。
    pub conversation_search: bool,
    pub tool_detail: bool,
    pub queue_control: bool,
    pub approval_queue: bool,
    pub child_task_view: bool,
    /// Runtime 是否已经完整装配 MCP 发现与调用网关。
    #[serde(default)]
    pub mcp_tools: bool,
    /// Runtime 是否已经完整装配 MCP 设置管理命令。
    #[serde(default)]
    pub mcp_management: bool,
    /// Runtime 是否已经支持可靠 Session Command 队列。
    #[serde(default)]
    pub session_commands: bool,
}

/// Desktop 首屏所需的稳定组合投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApplicationSnapshot {
    pub runtime_lifecycle: RuntimeLifecycle,
    pub configuration: ConfigurationStatus,
    pub models: Vec<ModelConfiguration>,
    /// 活动 Workspace，以及仍被当前 Session 绑定的已移除 Workspace。
    pub workspaces: Vec<WorkspaceSummary>,
    pub active_sessions: Vec<SessionSummary>,
    pub archived_sessions: Vec<SessionSummary>,
    /// 当前按稳定顺序选定的主控；创建失败或配置不可用时为 unavailable。
    #[serde(default)]
    pub controller_availability: crate::ControllerAvailabilitySnapshot,
    /// 除当前主控外还恢复出的 Controller 数量，只作为数据诊断。
    #[serde(default)]
    pub additional_controller_count: u64,
    pub capabilities: ApplicationCapabilities,
}

/// Conversation 的业务所有者。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationOwner {
    MainSession {
        session_id: SessionId,
    },
    ChildTask {
        session_id: SessionId,
        child_task_id: ChildTaskId,
    },
}

/// 一页可靠 Conversation 历史。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConversationPage {
    pub owner: ConversationOwner,
    /// cursor 所绑定的 Conversation 代次。
    pub generation: u64,
    /// 从旧到新排序的产品会话项。
    pub items: Vec<ConversationItem>,
    /// 用于继续向更早历史加载的不透明 cursor。
    pub previous_cursor: Option<String>,
    pub has_more: bool,
}

/// Conversation 中的用户消息、助手消息或已有 Context Summary 分割项。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    User(UserMessageSnapshot),
    Assistant(AssistantMessageSnapshot),
    /// Runtime 控制指令的可靠结算，不渲染为普通用户气泡。
    ControlResult {
        message_id: MessageId,
        result: crate::McpRefreshControlResultSnapshot,
    },
    /// 复用规范 Conversation 中已有的 Context Summary，只改变产品呈现。
    ContextSummary {
        message_id: MessageId,
        text: String,
    },
}

/// 可见 Input 的产品来源；旧记录与旧客户端缺省为真实用户。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationInputSourceSnapshot {
    /// 兼容未记录渠道信息的既有用户消息。
    #[default]
    User,
    /// Desktop 客户端直接提交的输入。
    Desktop {
        modality: InputModalitySnapshot,
        requested_output: OutputPreferenceSnapshot,
    },
    /// 已认证智能终端提交的输入；名称是消息接受时冻结的展示快照。
    Device {
        device_id: crate::DeviceId,
        device_name: String,
        modality: InputModalitySnapshot,
        requested_output: OutputPreferenceSnapshot,
    },
    ControllerDelivery {
        controller_session_id: SessionId,
        controller_run_id: RunId,
    },
    ProxyReport {
        source_session_id: SessionId,
        source_run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        source_goal_id: Option<GoalId>,
        source_run_status: RunStatus,
    },
}

/// Channel 输入已经由 Host 转换为正文后的业务模态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum InputModalitySnapshot {
    Text,
    SpeechTranscript,
}

/// 一轮 Channel 输入冻结的期望输出形态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum OutputPreferenceSnapshot {
    Text,
    Audio,
    TextAndAudio,
}

/// 可靠提交的用户消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UserMessageSnapshot {
    pub message_id: MessageId,
    /// 历史记录无法恢复归属时保持为空。
    pub input_id: Option<InputId>,
    pub text: String,
    pub attachment_ids: Vec<AttachmentId>,
    /// 用户随本条消息提交的有序冻结引用。
    #[serde(default)]
    pub quotes: Vec<QuotedTextSnapshot>,
    #[serde(default)]
    pub source: ConversationInputSourceSnapshot,
    /// 用户接受输入时冻结的单个 Skill 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub skill: Option<crate::SkillActivationTagSnapshot>,
    /// 用户随本条 Input 冻结的 MCP Server 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_selection: Option<crate::McpSelectionTagSnapshot>,
    pub created_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum QuotedTextSourceRoleSnapshot {
    User,
    Assistant,
}

/// Desktop 展示引用、提交和应用内来源定位所需的冻结内容。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct QuotedTextSnapshot {
    pub quote_id: PartId,
    pub exact: String,
    pub prefix: String,
    pub suffix: String,
    pub source_owner: ConversationOwner,
    pub source_generation: u64,
    pub source_message_id: MessageId,
    pub text_start_utf16: u32,
    pub text_end_utf16: u32,
    pub source_role: QuotedTextSourceRoleSnapshot,
    pub source_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_created_at_ms: Option<i64>,
    /// Runtime 接受输入时核对 direct locator 后写入；false 不影响冻结内容。
    #[serde(default)]
    pub source_available: bool,
}

/// 用户对助手消息的反馈。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum MessageFeedback {
    Positive,
    Negative,
}

/// 可靠提交的助手消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AssistantMessageSnapshot {
    pub message_id: MessageId,
    pub run_id: Option<RunId>,
    pub attempt: Option<u32>,
    /// 所属 Run 的可靠 step；旧记录缺失时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub step: Option<u32>,
    pub created_at_ms: Option<i64>,
    /// 所属 Run 的可靠结束时间；Turn 工具栏只在 Run 终结后展示该时间。
    pub finished_at_ms: Option<i64>,
    pub status: Option<RunStatus>,
    /// 按实际发生顺序排列，不生成额外 step 或摘要。
    pub segments: Vec<AssistantSegment>,
    pub usage: Option<TokenUsageSnapshot>,
    pub can_fork: bool,
    pub fork_point: Option<MessageId>,
    pub feedback: Option<MessageFeedback>,
}

/// 助手消息内保持原始顺序的显示片段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantSegment {
    Reasoning { part_id: PartId, text: String },
    Text { part_id: PartId, text: String },
    ToolGroup { tools: Vec<ToolEventSnapshot> },
}

/// 消息列表中的低干扰工具事件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ToolEventSnapshot {
    pub call_id: ToolCallId,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_identity: Option<crate::McpToolIdentity>,
    pub status: ToolActivityStatus,
    pub summary: Option<String>,
    /// 经过脱敏和结构化的输入；实时事件不携带，以快照为准。
    pub input: ToolInputSnapshot,
}

/// 工具详情中经过脱敏和结构化的输入。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolInputSnapshot {
    General {
        summary: String,
    },
    Delegation {
        title: String,
        task_summary: String,
    },
    File {
        operation: String,
        path: String,
    },
    Shell {
        command: String,
        working_directory: String,
        timeout_ms: u64,
        process_mode: String,
    },
    Mcp {
        identity: crate::McpToolIdentity,
        arguments_json: String,
    },
    ImageInspection {
        image_paths: Vec<String>,
        goal: String,
        background: Option<String>,
    },
    /// 老记录缺少可安全恢复的输入事实。
    Unavailable,
}

/// `inspect_images` 内部那一次辅助模型调用的可靠执行事实。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ImageInspectionDetailSnapshot {
    pub auxiliary_model: ModelKey,
    pub elapsed_ms: u64,
    pub usage: Option<TokenUsageSnapshot>,
}

/// 工具结果中可引用的文件。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ToolFileResourceOrigin {
    WorkspaceFile,
    SessionPrivateFile,
    SessionToolImage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ToolFileResourceState {
    Available,
    Unavailable,
}

/// 工具结果中可再次由 Runtime 解析的稳定文件引用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ToolFileReference {
    pub resource_ref_id: crate::ResourceRefId,
    pub origin: ToolFileResourceOrigin,
    pub display_name: String,
    pub display_path: Option<String>,
    pub size_bytes: Option<u64>,
    pub media_type: Option<String>,
    pub state: ToolFileResourceState,
}

/// 主会话中由可靠工具事件产生的文件引用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConversationFileReference {
    pub message_id: MessageId,
    pub call_id: ToolCallId,
    pub file: ToolFileReference,
}

/// Runtime 校验 Recall 不透明引用后提供给桌面端的安全导航目标。
///
/// WebView 只消费该投影，不接触也不解析带签名的 Recall reference。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RecallNavigationTarget {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
    pub lifecycle: crate::SessionLifecycle,
}

/// Recall 工具详情中的单条正文及其可选来源导航。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RecallToolDetailItem {
    pub content: String,
    pub role: Option<String>,
    pub created_at_ms: Option<i64>,
    pub navigation: Option<RecallNavigationTarget>,
}

/// Recall Source 的非致命失败；不阻止其他候选继续展示。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RecallToolDetailFailure {
    pub source_id: String,
    pub kind: String,
    pub message: String,
}

/// `recall_memory` 的桌面专用详情投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RecallToolDetailSnapshot {
    pub items: Vec<RecallToolDetailItem>,
    pub failures: Vec<RecallToolDetailFailure>,
    pub truncated: bool,
}

/// 点击工具事件后按需读取的详情。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ToolDetailSnapshot {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
    pub run_id: Option<RunId>,
    pub call_id: ToolCallId,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_identity: Option<crate::McpToolIdentity>,
    pub status: ToolActivityStatus,
    pub input: ToolInputSnapshot,
    /// 有界、格式化后的完整请求 JSON；用于详情代码块，不替代结构化输入投影。
    pub request_json: Option<String>,
    pub result_summary: Option<String>,
    /// 有界、格式化后的完整结果 JSON；纯文本结果保持为空。
    pub result_json: Option<String>,
    /// 仅 `recall_memory` 提供；引用已由 Runtime 校验并转换为安全导航目标。
    pub recall: Option<RecallToolDetailSnapshot>,
    /// 仅 `inspect_images` 成功结果提供；正文仍是直接文本 Tool Result。
    #[serde(default)]
    pub image_inspection: Option<ImageInspectionDetailSnapshot>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error: Option<RuntimeErrorInfo>,
    pub files: Vec<ToolFileReference>,
    pub output_truncated: bool,
    pub historical_fields_missing: bool,
}

/// Provider 已确认 token 的可缺省合计。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UsageTotals {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
}

/// 当前上下文窗口的占用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ContextUsageSnapshot {
    pub used_tokens: u64,
    pub window_tokens: u64,
    /// 0..=10000 表示 0.00%..=100.00%，避免 wire 浮点误差。
    pub usage_basis_points: u16,
}

/// 只属于主会话、不聚合子任务的用量投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionUsageSnapshot {
    pub accumulated: Option<UsageTotals>,
    pub previous_turn: Option<UsageTotals>,
    /// 最近一次主会话模型请求的缓存命中率；0..=10000 表示 0.00%..=100.00%。
    #[serde(default)]
    pub latest_cache_hit_basis_points: Option<u16>,
    /// 主会话全部模型请求按 token 加权后的缓存命中率。
    #[serde(default)]
    pub overall_cache_hit_basis_points: Option<u16>,
    /// 标题等不属于主 Agent Turn 的模型调用累计；尚无请求时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auxiliary: Option<AuxiliaryUsageSnapshot>,
    pub context: Option<ContextUsageSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AuxiliaryUsageSnapshot {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 一个子任务自身产生的用量；不与父会话或 sibling 聚合。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ChildTaskUsageSnapshot {
    pub accumulated: Option<UsageTotals>,
}

/// 主消息列表中连续任务树的一行稳定投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ChildTaskTreeItemSnapshot {
    pub task: ChildTaskSnapshot,
    pub usage: ChildTaskUsageSnapshot,
    pub pending_approval_count: u64,
    pub can_cancel: bool,
}

/// 进入子 Agent 二级消息列表所需的组合投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ChildTaskViewSnapshot {
    pub task: ChildTaskTreeItemSnapshot,
    pub approval_ids: Vec<ApprovalId>,
    pub conversation: ConversationPage,
}

/// 输入队列当前为何不继续自动串行。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum QueueExecutionState {
    Automatic,
    PausedByUser,
    ResumeRequired,
}

/// 一条尚未进入规范 Conversation 的输入。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct QueuedInputSnapshot {
    pub input_id: InputId,
    pub text_preview: String,
    pub submitted_at_ms: i64,
    pub position: u32,
    pub is_prioritized: bool,
    #[serde(default)]
    pub source: ConversationInputSourceSnapshot,
    /// Goal 存在时该用户输入只暂存于 Queue，必须由用户显式选择恢复或退出 Goal 后处理。
    #[serde(default)]
    pub held_by_goal: bool,
    /// 与 Input 一起冻结且不会被当前文件或开关替换的 Skill 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub skill: Option<crate::SkillActivationTagSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_selection: Option<crate::McpSelectionTagSnapshot>,
}

/// Session 的有序输入队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct QueueSnapshot {
    pub revision: u64,
    pub state: QueueExecutionState,
    pub items: Vec<crate::QueuedSessionItemSnapshot>,
}

/// Session 的有序审批队列；主、子 Agent 审批共享该队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApprovalQueueSnapshot {
    pub revision: u64,
    pub items: Vec<ApprovalSnapshot>,
    pub resolving_approval_id: Option<ApprovalId>,
}

/// 正式 Session 的 Workspace 当前名称与创建时冻结目录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionWorkspaceSnapshot {
    pub workspace_id: crate::WorkspaceId,
    /// Workspace 当前名称；编辑后可立即更新。
    pub label: String,
    /// Session 创建时冻结的主目录。
    pub primary_directory: String,
    /// Session 创建时冻结的有序附加目录。
    pub additional_directories: Vec<String>,
    /// 当前 Workspace 目录是否仍与 Session 冻结目录一致。
    pub directories_match_current: bool,
}

/// 主会话页面读取所需的组合投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionViewSnapshot {
    pub session: SessionSummary,
    /// 当前进程中正在执行的标题旁路调用；重启后由 pending 事实按需重建。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title_generation: Option<crate::SessionTitleGenerationSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace: Option<SessionWorkspaceSnapshot>,
    /// 发起 clear/compact 时使用的当前权威 Conversation generation。
    pub conversation_generation: u64,
    pub composer_capabilities: ComposerCapabilitiesSnapshot,
    #[serde(default)]
    pub work_plan: Option<WorkPlanSnapshot>,
    #[serde(default)]
    pub goal: Option<GoalSnapshot>,
    pub active_run: Option<RunSnapshot>,
    pub queue: QueueSnapshot,
    pub approvals: ApprovalQueueSnapshot,
    pub attachments: Vec<AttachmentSummary>,
    #[serde(default)]
    pub file_references: Vec<ConversationFileReference>,
    pub runs: Vec<RunSnapshot>,
    pub usage: SessionUsageSnapshot,
    pub child_tasks: Vec<ChildTaskTreeItemSnapshot>,
    #[serde(default)]
    pub skill_catalog: crate::SessionSkillCatalogSnapshot,
    #[serde(default)]
    pub active_skills: Vec<crate::ActiveSkillSnapshot>,
    pub conversation: ConversationPage,
}

/// 当前里程碑预留的查询与 mutation 请求/结果形状。
///
/// 这些类型先稳定跨层契约；对应 `RuntimeCommand` variant 在 M1 权威实现同时启用，避免 Host
/// 出现可路由却没有业务实现的命令。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetApplicationSnapshotRequest {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetApplicationSnapshotResult {
    pub snapshot: ObservedSnapshot<ApplicationSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetSessionViewRequest {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetSessionViewResult {
    pub snapshot: ObservedSnapshot<SessionViewSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetChildTaskViewRequest {
    pub session_id: SessionId,
    pub child_task_id: ChildTaskId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetChildTaskViewResult {
    pub snapshot: ObservedSnapshot<ChildTaskViewSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListConversationPageRequest {
    pub owner: ConversationOwner,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListConversationPageResult {
    pub snapshot: ObservedSnapshot<ConversationPage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationPageAroundRunRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationPageAroundRunResult {
    pub snapshot: ObservedSnapshot<ConversationPage>,
    pub anchor_message_id: MessageId,
}

/// Desktop 历史搜索的业务范围。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ConversationHistoryScope {
    /// 当前 Session 主会话及其全部子任务会话。
    Session,
    /// 与当前 Session 绑定到同一 Workspace 的全部会话。
    Workspace,
    /// Runtime 中全部仍可访问的活动及归档会话。
    Global,
}

/// 历史搜索命中的来源类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ConversationHistoryMatchKind {
    Title,
    Message,
}

/// 查询历史会话标题和正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SearchConversationHistoryRequest {
    /// 发起搜索的 Session，用于解析 Session/Workspace 范围。
    pub session_id: SessionId,
    pub query: String,
    pub scope: ConversationHistoryScope,
    /// 产品层分页偏移；客户端不得解析为存储序号。
    pub offset: u32,
    pub limit: u32,
}

/// 一条可导航的历史搜索命中。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConversationHistoryHit {
    pub owner: ConversationOwner,
    pub session_title: String,
    pub child_task_title: Option<String>,
    /// 标题命中没有具体消息锚点。
    pub message_id: Option<MessageId>,
    pub created_at_ms: Option<i64>,
    pub snippet: String,
    pub match_kind: ConversationHistoryMatchKind,
    pub lifecycle: SessionLifecycle,
}

/// 一页历史搜索结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SearchConversationHistoryResult {
    pub items: Vec<ConversationHistoryHit>,
    pub next_offset: Option<u32>,
    /// 部分派生索引仍在重建时，已就绪结果仍可展示。
    pub partial: bool,
    pub failed_owners: Vec<ConversationOwner>,
}

/// 读取命中消息附近、只用于用户查看的正文窗口。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationRecallWindowRequest {
    pub session_id: SessionId,
    pub owner: ConversationOwner,
    pub message_id: MessageId,
    pub before: u32,
    pub after: u32,
}

/// 一段只读搜索上下文；不会写入 Agent 上下文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationRecallWindowResult {
    pub owner: ConversationOwner,
    pub generation: u64,
    pub anchor_message_id: MessageId,
    pub items: Vec<ConversationItem>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

/// 按稳定 Message ID 读取包含目标消息的一页标准 Conversation。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationPageAroundMessageRequest {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetConversationPageAroundMessageResult {
    pub snapshot: ObservedSnapshot<ConversationPage>,
    pub anchor_message_id: MessageId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetToolDetailRequest {
    pub owner: ConversationOwner,
    pub message_id: MessageId,
    pub call_id: ToolCallId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetToolDetailResult {
    pub snapshot: ObservedSnapshot<ToolDetailSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PrioritizeQueuedInputRequest {
    pub session_id: SessionId,
    pub input_id: InputId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PrioritizeQueuedInputResult {
    pub queue: QueueSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct InterruptRunRequest {
    pub session_id: SessionId,
    pub run_id: RunId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct InterruptRunResult {
    pub run: RunSnapshot,
    pub queue: QueueSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResumeQueuedInputRequest {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_id: Option<InputId>,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResumeQueuedInputResult {
    pub queue: QueueSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RejectApprovalAndStopRunRequest {
    pub session_id: SessionId,
    pub approval_id: ApprovalId,
    pub expected_queue_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RejectApprovalAndStopRunResult {
    pub run: RunSnapshot,
    pub approvals: ApprovalQueueSnapshot,
}

/// 防止未来新增 credential 字段时静默进入 Desktop 生成类型。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_capabilities_default_new_mcp_fields_for_older_snapshots() {
        let capabilities = serde_json::from_value::<ApplicationCapabilities>(serde_json::json!({
            "conversation_paging": true,
            "conversation_search": true,
            "tool_detail": true,
            "queue_control": true,
            "approval_queue": true,
            "child_task_view": true
        }))
        .expect("deserialize older capability snapshot");

        assert!(!capabilities.mcp_tools);
        assert!(!capabilities.mcp_management);
        assert!(!capabilities.session_commands);
    }

    #[test]
    fn product_projection_schema_contains_no_credentials() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/src/generated/assistant-protocol.ts"
        ));
        for forbidden in ["access_token", "api_key:", "authorization"] {
            assert!(
                !source.to_ascii_lowercase().contains(forbidden),
                "generated bindings contain forbidden field {forbidden}"
            );
        }
    }

    #[test]
    fn conversation_item_preserves_segment_order_on_the_wire() {
        let message = AssistantMessageSnapshot {
            message_id: MessageId::new("message-1").expect("message id"),
            run_id: None,
            attempt: None,
            step: None,
            created_at_ms: None,
            finished_at_ms: None,
            status: None,
            segments: vec![
                AssistantSegment::Reasoning {
                    part_id: PartId::new("reasoning-1").expect("part id"),
                    text: "inspect".to_owned(),
                },
                AssistantSegment::Text {
                    part_id: PartId::new("text-1").expect("part id"),
                    text: "working".to_owned(),
                },
                AssistantSegment::ToolGroup { tools: Vec::new() },
                AssistantSegment::Reasoning {
                    part_id: PartId::new("reasoning-2").expect("part id"),
                    text: "verify".to_owned(),
                },
            ],
            usage: None,
            can_fork: false,
            fork_point: None,
            feedback: None,
        };

        let value = serde_json::to_value(message).expect("serialize message");
        let types = value["segments"]
            .as_array()
            .expect("segments")
            .iter()
            .map(|segment| segment["type"].as_str().expect("segment type"))
            .collect::<Vec<_>>();
        assert_eq!(types, ["reasoning", "text", "tool_group", "reasoning"]);
    }

    #[test]
    fn observed_conversation_page_round_trips_with_an_opaque_cursor() {
        let snapshot = ObservedSnapshot {
            observed_sequence: 17,
            value: ConversationPage {
                owner: ConversationOwner::MainSession {
                    session_id: SessionId::new("session-1").expect("session id"),
                },
                generation: 3,
                items: Vec::new(),
                previous_cursor: Some("opaque:do-not-parse".to_owned()),
                has_more: true,
            },
        };
        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(value["observed_sequence"], 17);
        assert_eq!(value["value"]["owner"]["type"], "main_session");
        assert_eq!(value["value"]["previous_cursor"], "opaque:do-not-parse");
        assert_eq!(
            serde_json::from_value::<ObservedSnapshot<ConversationPage>>(value)
                .expect("deserialize snapshot"),
            snapshot
        );
    }

    #[test]
    fn session_tool_image_origin_has_a_stable_public_wire_value() {
        let value = serde_json::to_value(ToolFileResourceOrigin::SessionToolImage)
            .expect("serialize origin");
        assert_eq!(value, "session_tool_image");
        assert_eq!(
            serde_json::from_value::<ToolFileResourceOrigin>(value).expect("deserialize origin"),
            ToolFileResourceOrigin::SessionToolImage
        );
    }
}
