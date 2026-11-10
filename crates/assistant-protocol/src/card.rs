//! 持久消息卡片与短期页面实例协议；页面载荷不进入模型对话或普通诊断。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{ConversationOwner, McpServerTarget, McpUiResource, MessageId, RunId};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageCardSource {
    Plugin {
        name: String,
        entry_id: String,
        published_sha256: String,
    },
    Mcp {
        target: McpServerTarget,
        resource_uri: String,
        entry_id: Option<String>,
        published_plugin_sha256: Option<String>,
    },
}

/// 查询只返回位置与展示身份；HTML、初始数据、私有结果和快照仅在受权挂载时读取。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MessageCardSummary {
    pub card_id: String,
    pub owner: ConversationOwner,
    pub run_id: Option<RunId>,
    pub anchor_message_id: MessageId,
    pub title: String,
    pub source: MessageCardSource,
    pub position: u64,
    pub snapshot_revision: u64,
    pub inherited: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListMessageCardsRequest {
    pub owner: ConversationOwner,
    pub message_ids: Vec<MessageId>,
    pub run_ids: Vec<RunId>,
    pub after: Option<u64>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListMessageCardsResult {
    pub cards: Vec<MessageCardSummary>,
    pub next: Option<u64>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MessageCardSnapshot {
    pub schema: String,
    #[ts(type = "unknown")]
    pub value: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MountMessageCardRequest {
    pub owner: ConversationOwner,
    pub card_id: String,
    /// 受信任客户端为当前连接生成的实例身份；不由内嵌页面提供。
    pub client_instance_id: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MountMessageCardResult {
    pub card: MessageCardSummary,
    pub instance_id: String,
    pub resource: McpUiResource,
    #[ts(type = "unknown")]
    pub arguments: Value,
    #[ts(type = "unknown")]
    pub tool_result: Value,
    pub snapshot: Option<MessageCardSnapshot>,
    pub writable_snapshot_schema: Option<String>,
    pub capabilities: Vec<crate::PluginUiCapability>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReleaseMessageCardRequest {
    pub owner: ConversationOwner,
    pub card_id: String,
    pub instance_id: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SaveMessageCardSnapshotRequest {
    pub owner: ConversationOwner,
    pub card_id: String,
    pub instance_id: String,
    pub expected_revision: u64,
    pub snapshot: MessageCardSnapshot,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SaveMessageCardSnapshotResult {
    /// false 表示 CAS 冲突；当前成功快照保持不变，由页面重读后处理。
    pub saved: bool,
    pub revision: u64,
    pub snapshot: Option<MessageCardSnapshot>,
}

macro_rules! private_payload_debug {
    ($($ty:ty),+ $(,)?) => { $(
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }
    )+ };
}

private_payload_debug!(
    MessageCardSnapshot,
    MountMessageCardResult,
    SaveMessageCardSnapshotRequest,
    SaveMessageCardSnapshotResult
);
