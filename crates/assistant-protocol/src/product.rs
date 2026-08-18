//! Desktop 使用的分页、组合查询与安全展示投影。
//!
//! 这些类型描述产品领域事实，不包含页面展开状态、像素布局、凭据或 Runtime 私有存储结构。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ApprovalId, ApprovalSnapshot, AttachmentId, AttachmentSummary, ChildTaskId, ChildTaskSnapshot,
    ConfigurationStatus, InputId, MessageId, ModelConfiguration, PartId, RunId, RunSnapshot,
    RunStatus, RuntimeErrorInfo, RuntimeLifecycle, SessionId, SessionLifecycle, SessionSummary,
    TokenUsageSnapshot, ToolActivityStatus, ToolCallId, WorkspaceSummary,
};

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
}

/// Desktop 首屏所需的稳定组合投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApplicationSnapshot {
    pub runtime_lifecycle: RuntimeLifecycle,
    pub configuration: ConfigurationStatus,
    pub models: Vec<ModelConfiguration>,
    pub workspaces: Vec<WorkspaceSummary>,
    pub active_sessions: Vec<SessionSummary>,
    pub archived_sessions: Vec<SessionSummary>,
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
    /// 从旧到新排序的消息。
    pub items: Vec<ConversationItem>,
    /// 用于继续向更早历史加载的不透明 cursor。
    pub previous_cursor: Option<String>,
    pub has_more: bool,
}

/// Conversation 中一条用户或助手消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    User(UserMessageSnapshot),
    Assistant(AssistantMessageSnapshot),
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
    pub created_at_ms: Option<i64>,
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
    /// 老记录缺少可安全恢复的输入事实。
    Unavailable,
}

/// 工具结果中可引用的文件。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ToolFileResourceOrigin {
    WorkspaceFile,
    SessionPrivateFile,
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
    pub status: ToolActivityStatus,
    pub input: ToolInputSnapshot,
    /// 有界、格式化后的完整请求 JSON；用于详情代码块，不替代结构化输入投影。
    pub request_json: Option<String>,
    pub result_summary: Option<String>,
    /// 有界、格式化后的完整结果 JSON；纯文本结果保持为空。
    pub result_json: Option<String>,
    /// 仅 `recall_memory` 提供；引用已由 Runtime 校验并转换为安全导航目标。
    pub recall: Option<RecallToolDetailSnapshot>,
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
    pub context: Option<ContextUsageSnapshot>,
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
}

/// Session 的有序输入队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct QueueSnapshot {
    pub revision: u64,
    pub state: QueueExecutionState,
    pub items: Vec<QueuedInputSnapshot>,
}

/// Session 的有序审批队列；主、子 Agent 审批共享该队列。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ApprovalQueueSnapshot {
    pub revision: u64,
    pub items: Vec<ApprovalSnapshot>,
    pub resolving_approval_id: Option<ApprovalId>,
}

/// 主会话页面读取所需的组合投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionViewSnapshot {
    pub session: SessionSummary,
    pub active_run: Option<RunSnapshot>,
    pub queue: QueueSnapshot,
    pub approvals: ApprovalQueueSnapshot,
    pub attachments: Vec<AttachmentSummary>,
    #[serde(default)]
    pub file_references: Vec<ConversationFileReference>,
    pub runs: Vec<RunSnapshot>,
    pub usage: SessionUsageSnapshot,
    pub child_tasks: Vec<ChildTaskTreeItemSnapshot>,
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
    pub input_id: InputId,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResumeQueuedInputResult {
    pub run: RunSnapshot,
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
}
