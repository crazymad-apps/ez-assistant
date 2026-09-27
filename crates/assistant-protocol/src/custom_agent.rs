//! 自定义智能体的完整配置、来源身份和本地内容 CAS 契约。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::PluginPackageSelection;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CustomAgentSource {
    Personal,
    Center { center_id: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CustomAgentSelection {
    pub name: String,
    pub source: CustomAgentSource,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AgentCapabilityReference {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CustomAgentConfig {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub skills: Vec<AgentCapabilityReference>,
    pub mcp: Vec<AgentCapabilityReference>,
}

/// 复用本地保存的完整配置与中心最后一次有效目录的只读投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CustomAgentSnapshot {
    pub source: CustomAgentSource,
    pub config: CustomAgentConfig,
    pub revision: String,
    pub created_at_ms: Option<u64>,
    pub updated_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CustomAgentListSnapshot {
    pub agents: Vec<CustomAgentSnapshot>,
    /// 当前企业来源不可读取时保留同用户最后有效目录，客户端可提示刷新失败。
    pub center_unavailable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SavePersonalAgentRequest {
    /// 创建时为 None；编辑时必须为上次读取的完整内容摘要。
    pub expected_revision: Option<String>,
    pub config: CustomAgentConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeletePersonalAgentRequest {
    pub name: String,
    pub expected_revision: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeletedPersonalAgentResult {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PrepareCustomAgentRequest {
    pub selection: CustomAgentSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<crate::WorkspaceId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum AgentDependencyStatus {
    Available,
    PluginMissing,
    PluginDisabled,
    CapabilityDisabled,
    CapabilityMissing,
    NameConflict,
    ConfigurationMissing,
    SoftwareMissing,
    RuntimeUnavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AgentDependencySnapshot {
    pub kind: String,
    pub reference: AgentCapabilityReference,
    pub status: AgentDependencyStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreparedCustomAgentSnapshot {
    pub selection: CustomAgentSelection,
    pub revision: String,
    pub dependencies: Vec<AgentDependencySnapshot>,
    /// 下载仍需由用户核对精确原包并确认；此结果不授权自动安装。
    pub install_candidates: Vec<PluginPackageSelection>,
}
