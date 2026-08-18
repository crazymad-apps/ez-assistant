//! Persona、Pinned Memory 与冻结 System Context 的产品投影。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::SessionId;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MemoryAttributeValue {
    String(String),
    Number(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum PinnedMemoryCreatedBy {
    User,
    AgentTool { session_id: SessionId },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PersonaSnapshot {
    pub enabled: bool,
    pub content: String,
    pub revision: u64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PinnedMemorySnapshot {
    pub id: String,
    pub category: String,
    pub content: String,
    pub attributes: BTreeMap<String, MemoryAttributeValue>,
    pub created_by: PinnedMemoryCreatedBy,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MemoryCapabilities {
    pub max_persona_bytes: u32,
    pub max_pinned_entries: u32,
    pub max_pinned_category_bytes: u32,
    pub max_pinned_content_bytes: u32,
    pub max_attributes_per_entry: u32,
    pub max_attribute_key_bytes: u32,
    pub max_attribute_string_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PinnedMemoryCollectionSnapshot {
    pub revision: u64,
    pub items: Vec<PinnedMemorySnapshot>,
    pub capabilities: MemoryCapabilities,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SystemContextSnapshot {
    pub session_id: SessionId,
    pub session_created_at_ms: Option<i64>,
    pub parts: Vec<String>,
}
