//! 规范对话落账 SPI。
//!
//! Core 以两阶段 tool exchange 提交自己产生的规范消息：
//!
//! 1. 工具副作用前 `begin_tool_exchange(AssistantMessage)`，持久化 pending；
//! 2. 每个已获 Allow 的调用在执行副作用前
//!    `mark_tool_execution_started(receipt, call_id)`；
//! 3. 整批调用结算后 `complete_tool_exchange(receipt, Vec<ToolMessage>)`，原子完成。
//!
//! pending exchange 是恢复事实，不是可投影的规范对话。Runtime 构建
//! `ConversationSnapshot` 时只投影 completed exchange；complete 失败时保留 pending，
//! 后续恢复必须补齐 interrupted/unknown 结果，不能暴露未配对 Tool Call。

use std::{future::Future, pin::Pin};

use agent_types::{AssistantMessage, ToolCallId, ToolMessage};
use thiserror::Error;

/// 一次 Recorder 调用的 Future。
pub type RecordFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, RecordError>> + Send + 'a>>;

/// 规范对话落账失败。
///
/// begin 失败阻断后续副作用；complete 失败使执行受控终止，但 Recorder 必须保留
/// 可恢复 pending exchange，不能写入部分 ToolResult。
#[derive(Clone, Debug, Error, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[error("failed to record conversation exchange: {message}")]
pub struct RecordError {
    /// 已脱敏的失败原因。
    pub message: String,
}

/// Recorder 为一个 pending tool exchange 返回的不透明标识。
///
/// Core 只原样回传给同一 Recorder，不解释其中内容；Runtime 可用自己的 journal
/// entry ID、事务序号或其他稳定标识实现。
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExchangeReceipt(String);

impl ExchangeReceipt {
    /// 从非空字符串创建 receipt。
    pub fn new(value: impl Into<String>) -> Result<Self, RecordError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(RecordError {
                message: "exchange receipt must not be empty".to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// 读取 Recorder 提供的不透明字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 规范对话的两阶段落账 SPI；实现归 Runtime（持久化会话/日志等）。
///
/// 对象安全，沿用 `ModelService` 的手写 boxed-future 模式（无 async-trait）。
pub trait ExecutionRecorder: Send + Sync {
    /// 工具副作用前持久化 pending exchange。
    ///
    /// 成功返回的 receipt 必须稳定指向该 pending exchange；失败时不得留下对规范
    /// 快照可见的 AssistantMessage。
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt>;

    /// 在指定 Tool Call 的任何外部副作用前可靠记录 execution started。
    ///
    /// 失败时 Core 必须跳过工具执行，并为该调用形成错误 Tool Result。receipt 与 call ID
    /// 必须属于同一 pending exchange；Recorder 不得把重复 started 当成第二次执行。
    fn mark_tool_execution_started<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        call_id: &'a ToolCallId,
    ) -> RecordFuture<'a, ()>;

    /// 原子写入完整、有序的 ToolMessage 批次并把 exchange 转为 completed。
    ///
    /// 失败时不得写入部分结果，pending exchange 必须保持可恢复。
    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()>;
}

/// completed exchange 的规范投影视图。
///
/// 该类型不再作为 Recorder 命令；Runtime/testkit 可用它把 completed exchange
/// 展平为 Assistant + Tool 消息序列。用户输入仍由 Runtime 自行落账。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ConversationDelta {
    /// 一个完整模型 Turn 聚合出的响应。
    Assistant(AssistantMessage),
    /// 一个已原子完成批次中的 ToolResult 消息。
    Tool(ToolMessage),
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
                metadata: None,
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
        let json = serde_json::to_value(ConversationDelta::Tool(sample_tool_message()))
            .expect("serialize delta to value");
        assert_eq!(json["type"], "tool");
    }

    #[test]
    fn receipt_and_error_round_trip_serde() {
        let receipt = ExchangeReceipt::new("exchange_1").expect("valid receipt");
        let json = serde_json::to_string(&receipt).expect("serialize receipt");
        assert_eq!(
            serde_json::from_str::<ExchangeReceipt>(&json).expect("deserialize receipt"),
            receipt
        );
        assert!(ExchangeReceipt::new("  ").is_err());

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
            "failed to record conversation exchange: disk is full"
        );
    }
}
