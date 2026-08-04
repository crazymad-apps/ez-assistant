//! 调试查看器共享线格式：推送端与 viewer server 之间的 JSON 契约。
//!
//! 数据流：推送端（CLI demo、将来的 Runtime）→ `POST /ingest` → viewer server
//! → SSE `/events` 广播 → 浏览器。线格式归本 crate 所有，不进入 assistant-protocol。
//! credential 永不进入任何 payload；消息正文属于调试内容，由本地 loopback 承载。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_core::AgentEvent;
use agent_model::{ModelEvent, ModelRequest};
use serde::{Deserialize, Serialize};

/// viewer server 的默认端口。
pub const DEFAULT_PORT: u16 = 7331;

/// 推送给 viewer 的调试信封。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DebugEnvelope {
    /// 数据通道：模型、Agent 执行或 Runtime 编排。
    pub ch: DebugChannel,
    /// 推送端侧单调递增序号，用于发现丢消息。
    pub seq: u64,
    /// 推送端发送时刻（Unix 毫秒）。
    pub sent_at_ms: u64,
    /// 调用方关联 ID（对应 `TraceContext::correlation_id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// 具体调试内容。
    pub payload: DebugPayload,
}

/// 调试数据通道。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugChannel {
    /// 模型数据流（模型事件、建立/失败元信息）。
    Llm,
    /// Agent Core 单次执行事件。
    Agent,
    /// Runtime 的 Run、Session 与 Journal 编排事件。
    Runtime,
}

/// 调试内容。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugPayload {
    /// 发往 Provider 的规范请求快照（用户输入与上下文原文）。
    TurnRequested {
        /// 规范请求原文。
        request: ModelRequest,
    },
    /// 一次模型 Turn 建立成功（收到响应头、事件流开始）。
    TurnEstablished {
        /// 实际模型名称。
        model: String,
        /// 请求 endpoint。
        endpoint: String,
        /// 本次请求携带的消息数。
        message_count: u32,
        /// 本次请求携带的工具数。
        tool_count: u32,
        /// 从发起 `stream` 到建立成功的耗时。
        elapsed_ms: u64,
    },
    /// 规范模型事件原文。
    ModelEvent {
        /// 规范模型事件。
        event: ModelEvent,
    },
    /// 建立前失败（`ModelService::stream` 的 `Err` 路径）。
    EstablishmentFailed {
        /// 已脱敏的错误文本。
        error: String,
    },
    /// Agent Core 发出的强类型执行事件。
    AgentEvent {
        /// 原始 Agent 执行事件。
        event: AgentEvent,
    },
    /// Runtime 编排事件。
    RuntimeEvent {
        /// 稳定事件名称。
        name: String,
        /// 事件的结构化数据；不得包含 credential。
        data: serde_json::Value,
    },
}

/// server 广播给浏览器的消息：信封 + server 接收时刻。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BroadcastMessage {
    /// 原始调试信封（字段平铺）。
    #[serde(flatten)]
    pub envelope: DebugEnvelope,
    /// server 接收时刻（Unix 毫秒）。
    pub received_at_ms: u64,
}

/// 当前 Unix 毫秒；系统时钟异常时返回 0。
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use agent_model::{ModelError, ModelTransportErrorKind};
    use agent_types::{MessageId, ModelIdentity, ProviderId};
    use serde_json::json;

    use super::*;

    #[test]
    fn envelope_round_trips_with_model_event_payload() {
        let envelope = DebugEnvelope {
            ch: DebugChannel::Llm,
            seq: 7,
            sent_at_ms: 1_752_000_000_000,
            correlation_id: Some("chat-1".to_owned()),
            payload: DebugPayload::ModelEvent {
                event: ModelEvent::TurnStarted {
                    message_id: MessageId::new("chatcmpl_1").expect("valid message id"),
                    model: ModelIdentity::new(
                        ProviderId::new("deepseek").expect("valid provider id"),
                        "deepseek-v4-flash",
                    ),
                },
            },
        };
        let json = serde_json::to_value(&envelope).expect("serialize envelope");
        assert_eq!(json["ch"], json!("llm"));
        assert_eq!(json["payload"]["kind"], json!("model_event"));
        assert_eq!(
            json["payload"]["event"]["TurnStarted"]["model"]["model"],
            json!("deepseek-v4-flash")
        );
        let decoded: DebugEnvelope = serde_json::from_value(json).expect("deserialize envelope");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn structured_model_error_round_trips_in_model_event_payload() {
        let envelope = DebugEnvelope {
            ch: DebugChannel::Llm,
            seq: 8,
            sent_at_ms: 1_752_000_000_001,
            correlation_id: Some("chat-2".to_owned()),
            payload: DebugPayload::ModelEvent {
                event: ModelEvent::TurnFailed {
                    error: ModelError::Transport {
                        kind: ModelTransportErrorKind::Interrupted,
                        message: "connection reset".to_owned(),
                    },
                },
            },
        };

        let json = serde_json::to_value(&envelope).expect("serialize envelope");
        assert_eq!(
            json["payload"]["event"]["TurnFailed"]["error"]["Transport"]["kind"],
            json!("interrupted")
        );
        assert_eq!(
            serde_json::from_value::<DebugEnvelope>(json).expect("deserialize envelope"),
            envelope
        );
    }

    #[test]
    fn turn_requested_payload_round_trips() {
        let request = ModelRequest {
            system: agent_model::SystemPromptSnapshot::default(),
            conversation: agent_types::ConversationSnapshot::new(vec![
                agent_types::ConversationMessage::User(agent_types::UserMessage {
                    id: MessageId::new("user_1").expect("valid message id"),
                    parts: vec![agent_types::UserPart::Text(agent_types::TextPart {
                        id: agent_types::PartId::new("user_1_text").expect("valid part id"),
                        text: "你好".to_owned(),
                    })],
                }),
            ]),
            tools: vec![],
            tool_choice: agent_types::ToolChoice::Auto,
            generation: agent_model::GenerationConfig::default(),
            reasoning: None,
            provider_options: agent_model::ProviderOptions::new(),
        };
        let payload = DebugPayload::TurnRequested {
            request: request.clone(),
        };
        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert_eq!(json["kind"], json!("turn_requested"));
        let message = &json["request"]["conversation"]["messages"][0];
        assert_eq!(message["role"], json!("user"));
        assert_eq!(message["turn"]["parts"][0]["data"]["text"], json!("你好"));
        let decoded: DebugPayload = serde_json::from_value(json).expect("deserialize payload");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn all_channels_and_layer_payloads_round_trip() {
        for (channel, payload, expected_kind) in [
            (
                DebugChannel::Agent,
                DebugPayload::AgentEvent {
                    event: AgentEvent::ExecutionStarted,
                },
                "agent_event",
            ),
            (
                DebugChannel::Runtime,
                DebugPayload::RuntimeEvent {
                    name: "run_started".to_owned(),
                    data: json!({"session_id": "session-1", "run_id": "run-1"}),
                },
                "runtime_event",
            ),
        ] {
            let envelope = DebugEnvelope {
                ch: channel,
                seq: 9,
                sent_at_ms: 1_752_000_000_000,
                correlation_id: Some("session-1/run-1".to_owned()),
                payload,
            };
            let json = serde_json::to_value(&envelope).expect("serialize envelope");
            assert_eq!(json["payload"]["kind"], expected_kind);
            assert_eq!(
                serde_json::from_value::<DebugEnvelope>(json).expect("deserialize envelope"),
                envelope
            );
        }

        let channels = [
            (DebugChannel::Llm, "llm"),
            (DebugChannel::Agent, "agent"),
            (DebugChannel::Runtime, "runtime"),
        ];
        for (channel, expected) in channels {
            assert_eq!(
                serde_json::to_value(channel).expect("serialize channel"),
                expected
            );
        }
    }

    #[test]
    fn broadcast_message_flattens_envelope_fields() {
        let message = BroadcastMessage {
            envelope: DebugEnvelope {
                ch: DebugChannel::Llm,
                seq: 1,
                sent_at_ms: 1,
                correlation_id: None,
                payload: DebugPayload::EstablishmentFailed {
                    error: "auth failed".to_owned(),
                },
            },
            received_at_ms: 2,
        };
        let json = serde_json::to_value(&message).expect("serialize broadcast");
        assert_eq!(json["payload"]["kind"], json!("establishment_failed"));
        assert_eq!(json["received_at_ms"], json!(2));
        // correlation_id 缺省时不出现，浏览器端反序列化回 None。
        assert!(json.get("correlation_id").is_none());
        let decoded: BroadcastMessage =
            serde_json::from_value(json).expect("deserialize broadcast");
        assert_eq!(decoded, message);
    }
}
