//! 规范对话落账 SPI。
//!
//! Core 只提交自己产生的增量（[`ConversationDelta`]）；用户输入由 Runtime
//! 自行落账。record 失败会阻断后续副作用并使执行受控终止。

use std::{future::Future, pin::Pin};

use agent_types::{AssistantMessage, ToolMessage};
use thiserror::Error;

/// 一次落账调用的 Future。
pub type RecordFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RecordReceipt, RecordError>> + Send + 'a>>;

/// 规范对话的落账 SPI；实现归 Runtime（持久化会话/日志等）。
///
/// 对象安全，沿用 `ModelService` 的手写 boxed-future 模式（无 async-trait）。
pub trait ExecutionRecorder: Send + Sync {
    /// 提交一个对话增量；`Err` 阻断后续副作用并受控终止执行。
    fn record<'a>(&'a self, delta: ConversationDelta) -> RecordFuture<'a>;
}

/// Core 提交给 Recorder 的对话增量。
///
/// 只包含 Core 自己产生的两类消息；用户输入不在其中（由 Runtime 自行落账）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ConversationDelta {
    /// 一个完整模型 Turn 聚合出的响应。
    Assistant(AssistantMessage),
    /// 一批 Tool Call 结算出的结果消息。
    Tool(ToolMessage),
}

/// 落账空确认；语义即"已接收"，不携带任何信息。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordReceipt;

/// 落账失败；任何失败都阻断后续副作用并使执行受控终止。
#[derive(Clone, Debug, Error, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[error("failed to record conversation delta: {message}")]
pub struct RecordError {
    /// 已脱敏的失败原因。
    pub message: String,
}

#[cfg(test)]
mod tests {
    use agent_types::{
        FinishReason, MessageId, ModelIdentity, ProviderId, ToolCallId, ToolResult,
        ToolResultContent, ToolResultStatus,
    };

    use super::*;

    fn sample_assistant_message() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn sample_tool_message() -> ToolMessage {
        ToolMessage {
            id: MessageId::new("toolmsg_1").expect("valid message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call_1").expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("2026-07-27".to_owned()),
            },
        }
    }

    #[test]
    fn conversation_delta_round_trips_serde() {
        let deltas = vec![
            ConversationDelta::Assistant(sample_assistant_message()),
            ConversationDelta::Tool(sample_tool_message()),
        ];
        for delta in deltas {
            let json = serde_json::to_string(&delta).expect("serialize delta");
            assert_eq!(
                serde_json::from_str::<ConversationDelta>(&json).expect("deserialize delta"),
                delta
            );
        }
        // 稳定 tag：assistant/tool 蛇形命名。
        let json = serde_json::to_value(ConversationDelta::Tool(sample_tool_message()))
            .expect("serialize delta to value");
        assert_eq!(json["type"], "tool");
    }

    #[test]
    fn receipt_and_error_round_trip_serde() {
        let json = serde_json::to_string(&RecordReceipt).expect("serialize receipt");
        assert_eq!(
            serde_json::from_str::<RecordReceipt>(&json).expect("deserialize receipt"),
            RecordReceipt
        );

        let error = RecordError {
            message: "disk is full".to_owned(),
        };
        let json = serde_json::to_string(&error).expect("serialize error");
        assert_eq!(
            serde_json::from_str::<RecordError>(&json).expect("deserialize error"),
            error
        );
        assert_eq!(
            error.to_string(),
            "failed to record conversation delta: disk is full"
        );
    }
}
