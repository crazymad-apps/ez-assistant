use agent_types::{
    AssistantMessage, MessageId, ModelIdentity, PartId, TokenUsage, ToolCallId, ToolName,
};
use serde_json::Value;

use crate::ModelError;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
/// 一次 Provider Turn 的规范流式事件。
///
/// 生命周期契约（由 [`crate::LifecycleValidator`] 强制执行）：
///
/// - 首个事件必须是 `TurnStarted`；`TurnFailed` 允许在任何阶段出现。
/// - 每个 Part 的 `Started`/`Delta`/`Finished` 按 ID 配对；不同 Part 可以交错。
/// - Tool arguments 分片到达，但 `ToolCallFinished` 必须携带已拼完并通过
///   JSON 解析的完整值；组装失败的生产方必须以 `TurnFailed` 结束流。
/// - 每个成功建立的流恰好以一个 `TurnFinished` 或 `TurnFailed` 结束；
///   终态之后到达的数据一律被拒绝。
/// - `OpaqueProviderState` 没有独立流事件，只出现在最终 `AssistantMessage` 中。
pub enum ModelEvent {
    /// 流建立后的第一个事件，声明本次响应的消息身份。
    TurnStarted {
        /// 本次响应对应的规范消息 ID；`TurnFinished` 中的消息必须与之相同。
        message_id: MessageId,
        /// 实际生成响应的模型身份。
        model: ModelIdentity,
    },
    /// 一个 reasoning 片段开始。
    ReasoningStarted {
        /// 片段 ID。
        id: PartId,
    },
    /// reasoning 文本增量。
    ReasoningDelta {
        /// 片段 ID。
        id: PartId,
        /// 本次到达的 reasoning 文本片段。
        delta: String,
    },
    /// 一个 reasoning 片段结束。
    ReasoningFinished {
        /// 片段 ID。
        id: PartId,
    },
    /// 一个正文片段开始。
    TextStarted {
        /// 片段 ID。
        id: PartId,
    },
    /// 正文文本增量。
    TextDelta {
        /// 片段 ID。
        id: PartId,
        /// 本次到达的正文文本片段。
        delta: String,
    },
    /// 一个正文片段结束。
    TextFinished {
        /// 片段 ID。
        id: PartId,
    },
    /// 一次工具调用开始。
    ToolCallStarted {
        /// Provider 分配的调用 ID。
        id: ToolCallId,
        /// 要调用的工具名称。
        name: ToolName,
    },
    /// 工具参数增量；是尚未拼装完成的 JSON 片段，不是完整 JSON 值。
    ToolCallDelta {
        /// 调用 ID。
        id: ToolCallId,
        /// 本次到达的参数 JSON 片段。
        arguments_delta: String,
    },
    /// 一次工具调用结束；参数已拼装并通过 JSON 解析。
    ToolCallFinished {
        /// 调用 ID。
        id: ToolCallId,
        /// 完整的工具参数 JSON 值。
        arguments: Value,
    },
    /// Provider 报告的 token 用量快照；可多次到达，后者覆盖前者。
    UsageUpdated {
        /// 最新用量快照。
        usage: TokenUsage,
    },
    /// 唯一正常终态，携带按 Provider 输出顺序完整聚合的规范响应。
    TurnFinished {
        /// 完整聚合的 AssistantMessage；finish reason 与最终 usage 均在其中。
        message: AssistantMessage,
    },
    /// 唯一异常终态；流建立后的所有失败（含取消）都以此受控结束。
    TurnFailed {
        /// 已脱敏的失败原因。
        error: ModelError,
    },
}

impl ModelEvent {
    /// 是否为终态事件（`TurnFinished` 或 `TurnFailed`）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ModelEvent::TurnFinished { .. } | ModelEvent::TurnFailed { .. }
        )
    }
}
