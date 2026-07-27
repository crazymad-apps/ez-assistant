//! 执行事件与背压事件通道。
//!
//! 事件不是事实源（UI 断线后以规范对话快照恢复），论证为可安全丢弃。背压策略：
//!
//! - bounded `tokio::sync::mpsc`，容量 [`AGENT_EVENT_CHANNEL_CAPACITY`]；
//! - 发送端 `try_send`：通道满即丢弃并用原子计数器计数，终态事件携带
//!   `dropped_events`；
//! - 订阅断开（receiver dropped）不影响执行，发送方不阻塞、不 panic。

use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use agent_tools::ToolOutputChannel;
use agent_types::{AssistantMessage, PartId, ToolCall, ToolCallId};
use futures_core::Stream;
use tokio::sync::mpsc;

use crate::ExecutionError;

/// 事件通道容量；超出后新事件丢弃并计数。
pub const AGENT_EVENT_CHANNEL_CAPACITY: usize = 256;

/// 一次 Agent 执行对外发出的规范事件。
///
/// 生命周期：首个事件为 `ExecutionStarted`，恰好以一个终态事件
/// （`ExecutionCompleted` / `ExecutionFailed` / `ExecutionCancelled`）结束。
/// 事件只面向 UI/诊断观察，规范对话以 Recorder 落账为准。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 执行开始。
    ExecutionStarted,
    /// 一个模型 Turn（step）开始。
    StepStarted {
        /// 当前模型 Turn 序号。
        step: u32,
    },
    /// 正文文本增量。
    TextDelta {
        /// 片段 ID。
        id: PartId,
        /// 本次到达的正文文本片段。
        delta: String,
    },
    /// reasoning 文本增量。
    ReasoningDelta {
        /// 片段 ID。
        id: PartId,
        /// 本次到达的 reasoning 文本片段。
        delta: String,
    },
    /// 模型请求执行一个工具（进入授权闸前）。
    ToolProposed {
        /// 完整组装出的规范 Tool Call。
        call: ToolCall,
    },
    /// 一个工具调用开始执行（已过闸）。
    ToolStarted {
        /// 调用 ID。
        call_id: ToolCallId,
    },
    /// 工具执行过程中的流式输出片段。
    ToolOutput {
        /// 调用 ID。
        call_id: ToolCallId,
        /// 输出通道。
        channel: ToolOutputChannel,
        /// 面向观察者的增量文本。
        chunk: String,
    },
    /// 一个工具调用完成（成功或失败）。
    ToolCompleted {
        /// 调用 ID。
        call_id: ToolCallId,
        /// 完成状态。
        status: ToolCompletionStatus,
    },
    /// 唯一正常终态，携带最终聚合的规范响应。
    ExecutionCompleted {
        /// 最终 AssistantMessage。
        message: AssistantMessage,
    },
    /// 异常终态；模型失败、落账失败、预算到达等受控终止。
    ExecutionFailed {
        /// 已脱敏的失败原因。
        error: ExecutionError,
        /// 本次执行因背压或订阅断开丢弃的事件数。
        dropped_events: u64,
    },
    /// 取消终态；收敛前执行内所有未结算调用已先行结算。
    ExecutionCancelled {
        /// 本次执行因背压或订阅断开丢弃的事件数。
        dropped_events: u64,
    },
}

/// 一次工具调用的完成状态；与 `agent_types::ToolResultStatus` 的成败语义对齐。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCompletionStatus {
    /// 工具执行成功。
    Success,
    /// 工具执行失败（含输入校验失败、执行错误、被拒绝与预算/取消结算）。
    Failed,
}

impl AgentEvent {
    /// 是否为终态事件（`ExecutionCompleted` / `ExecutionFailed` / `ExecutionCancelled`）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::ExecutionCompleted { .. }
                | Self::ExecutionFailed { .. }
                | Self::ExecutionCancelled { .. }
        )
    }
}

/// 创建执行事件通道：bounded mpsc（[`AGENT_EVENT_CHANNEL_CAPACITY`]）+
/// `try_send` 丢弃计数；drop 接收端不影响发送方。
pub fn agent_event_channel() -> (AgentEventSender, AgentEventStream) {
    let (sender, receiver) = mpsc::channel(AGENT_EVENT_CHANNEL_CAPACITY);
    (
        AgentEventSender {
            sender,
            dropped_events: Arc::new(AtomicU64::new(0)),
        },
        AgentEventStream { receiver },
    )
}

/// 事件发送端（引擎侧持有）；发送永不阻塞、订阅断开不 panic。
#[derive(Debug)]
pub struct AgentEventSender {
    sender: mpsc::Sender<AgentEvent>,
    dropped_events: Arc<AtomicU64>,
}

impl AgentEventSender {
    /// 非阻塞发送一个事件；通道满或订阅断开时丢弃并计数。
    pub fn send(&self, event: AgentEvent) {
        if self.sender.try_send(event).is_err() {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 截至当前未能投递（通道满或订阅断开）的事件数；终态事件的
    /// `dropped_events` 字段由引擎以此填充。
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }
}

/// 事件接收流；drop（订阅断开）不影响执行。
#[derive(Debug)]
pub struct AgentEventStream {
    receiver: mpsc::Receiver<AgentEvent>,
}

impl Stream for AgentEventStream {
    type Item = AgentEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{FinishReason, MessageId, ModelIdentity, ProviderId, ToolName};

    use super::*;
    use crate::{BudgetKind, testutil::next};

    fn part_id() -> PartId {
        PartId::new("text_1").expect("valid part id")
    }

    fn call_id() -> ToolCallId {
        ToolCallId::new("call_1").expect("valid call id")
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: call_id(),
            name: ToolName::new("read_file").expect("valid tool name"),
            arguments: serde_json::json!({"path": "/tmp/a.txt"}),
        }
    }

    fn assistant_message() -> AssistantMessage {
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

    #[test]
    fn every_event_variant_round_trips_serde() {
        let events = vec![
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::TextDelta {
                id: part_id(),
                delta: "hello".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                id: part_id(),
                delta: "thinking".to_owned(),
            },
            AgentEvent::ToolProposed { call: tool_call() },
            AgentEvent::ToolStarted { call_id: call_id() },
            AgentEvent::ToolOutput {
                call_id: call_id(),
                channel: ToolOutputChannel::Stderr,
                chunk: "chunk".to_owned(),
            },
            AgentEvent::ToolCompleted {
                call_id: call_id(),
                status: ToolCompletionStatus::Failed,
            },
            AgentEvent::ExecutionCompleted {
                message: assistant_message(),
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::Steps,
                    limit: 4,
                },
                dropped_events: 3,
            },
            AgentEvent::ExecutionCancelled { dropped_events: 0 },
        ];
        for event in events {
            let json = serde_json::to_string(&event).expect("serialize event");
            assert_eq!(
                serde_json::from_str::<AgentEvent>(&json).expect("deserialize event"),
                event
            );
        }
        // 稳定 tag：蛇形命名；channel/status 枚举同为蛇形。
        let json =
            serde_json::to_value(AgentEvent::ExecutionStarted).expect("serialize event to value");
        assert_eq!(json, serde_json::json!({"type": "execution_started"}));
        let json = serde_json::to_value(AgentEvent::ToolOutput {
            call_id: call_id(),
            channel: ToolOutputChannel::Stdout,
            chunk: "out".to_owned(),
        })
        .expect("serialize event to value");
        assert_eq!(json["type"], "tool_output");
        assert_eq!(json["channel"], "stdout");
        let json = serde_json::to_value(AgentEvent::ToolCompleted {
            call_id: call_id(),
            status: ToolCompletionStatus::Success,
        })
        .expect("serialize event to value");
        assert_eq!(json["status"], "success");
    }

    #[test]
    fn terminal_events_are_exactly_the_three_end_states() {
        let terminals = vec![
            AgentEvent::ExecutionCompleted {
                message: assistant_message(),
            },
            AgentEvent::ExecutionFailed {
                error: ExecutionError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    limit: 2,
                },
                dropped_events: 0,
            },
            AgentEvent::ExecutionCancelled { dropped_events: 0 },
        ];
        for event in terminals {
            assert!(event.is_terminal());
        }
        for event in [
            AgentEvent::ExecutionStarted,
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::ToolStarted { call_id: call_id() },
        ] {
            assert!(!event.is_terminal());
        }
    }

    #[test]
    fn channel_delivers_events_in_order() {
        let (sender, mut stream) = agent_event_channel();
        sender.send(AgentEvent::ExecutionStarted);
        sender.send(AgentEvent::StepStarted { step: 1 });
        assert_eq!(next(&mut stream), Some(AgentEvent::ExecutionStarted));
        assert_eq!(next(&mut stream), Some(AgentEvent::StepStarted { step: 1 }));
        assert_eq!(sender.dropped_events(), 0);
        drop(sender);
        assert_eq!(next(&mut stream), None);
    }

    #[test]
    fn full_channel_drops_and_counts_events() {
        let (sender, mut stream) = agent_event_channel();
        for _ in 0..AGENT_EVENT_CHANNEL_CAPACITY {
            sender.send(AgentEvent::ExecutionStarted);
        }
        assert_eq!(sender.dropped_events(), 0);
        // 通道已满：新事件丢弃并计数，发送方不阻塞。
        sender.send(AgentEvent::ExecutionStarted);
        sender.send(AgentEvent::ExecutionStarted);
        assert_eq!(sender.dropped_events(), 2);

        // 已入队的事件一个不少、按序可取。
        for _ in 0..AGENT_EVENT_CHANNEL_CAPACITY {
            assert_eq!(next(&mut stream), Some(AgentEvent::ExecutionStarted));
        }
        assert_eq!(sender.dropped_events(), 2);
    }

    #[test]
    fn dropped_receiver_never_blocks_or_panics_sender() {
        let (sender, stream) = agent_event_channel();
        drop(stream);
        // 订阅断开：发送方不阻塞、不 panic，事件计入丢弃数。
        sender.send(AgentEvent::ExecutionStarted);
        sender.send(AgentEvent::ExecutionCancelled { dropped_events: 0 });
        assert_eq!(sender.dropped_events(), 2);
    }
}
