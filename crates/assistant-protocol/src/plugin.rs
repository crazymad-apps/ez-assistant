//! 用户域插件包变更的产品协议；安装操作是可查询的持久收据。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::IdempotencyKey;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PluginArchiveFormat {
    Zip,
    TarGzip,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginPackageOrigin {
    Upload,
    Center { center_id: String },
}

/// 用户确认的单个精确原包；Host 不会擅自换版本或扩大下载大小。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginPackageSelection {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub format: PluginArchiveFormat,
    pub origin: PluginPackageOrigin,
    pub expected_active_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct BeginPluginInstallRequest {
    pub key: IdempotencyKey,
    /// 按此顺序逐项准备和提交；前一项收尾后才开始下一项。
    pub packages: Vec<PluginPackageSelection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UninstallPluginRequest {
    pub key: IdempotencyKey,
    pub name: String,
    pub expected_active_sha256: String,
    pub clear_data: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginOperationRequest {
    pub key: IdempotencyKey,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PluginOperationStatus {
    Accepted,
    Preparing,
    Committing,
    Cleaning,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginOperationSnapshot {
    pub key: IdempotencyKey,
    pub kind: PluginOperationKind,
    pub phase: PluginOperationStatus,
    pub committed_names: Vec<String>,
    pub cancel_requested: bool,
    pub error_code: Option<String>,
    pub cleanup_required: bool,
    /// 仅当前进程中的实时接收进度；重启后为 None，不伪造历史字节数。
    pub received_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PluginOperationKind {
    Install,
    Update,
    Uninstall,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct InstalledPluginSnapshot {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub origin: PluginPackageOrigin,
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub enabled: bool,
    pub package_state: PluginPackageState,
    /// Host 内原始设置内容的摘要，用于双窗口过期覆盖保护。
    pub settings_revision: String,
    pub configuration: Vec<PluginConfigurationFieldSnapshot>,
    pub external_software: Vec<PluginSoftwareFieldSnapshot>,
    /// 包清单声明的能力；实际可用性由 Skill/MCP 目录及其诊断决定。
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginConfigurationFieldSnapshot {
    pub name: String,
    pub label: String,
    pub required: bool,
    pub secret: bool,
    pub is_set: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginSoftwareFieldSnapshot {
    pub name: String,
    pub display_name: String,
    pub is_set: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MutatePluginSettingsRequest {
    pub name: String,
    pub expected_active_sha256: String,
    pub expected_revision: String,
    pub mutation: PluginSettingsMutation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginSettingsMutation {
    SetEnabled { enabled: bool },
    SetParameter { name: String, value: Option<String> },
    SetSoftwarePath { name: String, value: Option<String> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct MutatePluginSettingsResult {
    pub plugin: InstalledPluginSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginConfigFileSnapshot {
    pub path: String,
    pub revision: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginConfigFilesSnapshot {
    pub files: Vec<PluginConfigFileSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum PluginPackageState {
    Available,
    Corrupt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginListSnapshot {
    pub plugins: Vec<InstalledPluginSnapshot>,
    pub cleanup_blocked: bool,
    #[serde(default)]
    pub operations: Vec<PluginOperationSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ResolvePluginPackageRequest {
    pub name: String,
    pub expected_active_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginMarketVersion {
    pub version: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub archive_format: PluginArchiveFormat,
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginMarketEntry {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub versions: Vec<PluginMarketVersion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PluginMarketSnapshot {
    pub plugins: Vec<PluginMarketEntry>,
}
