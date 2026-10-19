//! 本机 Host 登记管理；中心凭据和授权集合不向客户端暴露。
use crate::RuntimeErrorInfo;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HostRegistrationCommand {
    GetStatus,
    Apply { name: String },
    Withdraw,
    Exit,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum HostRegistrationIntent {
    Publish,
    Withdraw,
    Exit,
    Closed,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum HostRegistrationState {
    Pending,
    Registered,
    Rejected,
    Withdrawn,
    Revoked,
}
impl HostRegistrationState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::Withdrawn | Self::Revoked)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct HostRegistrationDevice {
    pub id: String,
    pub installation_id: String,
    pub name: String,
    pub platform: String,
    pub host_version: String,
    pub status: HostRegistrationState,
    pub enabled: bool,
    pub online: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct HostRegistrationStatus {
    pub center_id: Option<String>,
    pub installation_id: Option<String>,
    pub registration_id: Option<String>,
    pub intent: Option<HostRegistrationIntent>,
    /// 本次中心查询的结果；error 非空时未知，不使用本机缓存冒充最新事实。
    pub device: Option<HostRegistrationDevice>,
    pub error: Option<RuntimeErrorInfo>,
    #[serde(default)]
    pub connection: HostNetworkState,
}

/// 本机 Host 到中心的设备控制连接；不代表业务访问或媒体可用。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum HostNetworkState {
    #[default]
    Disconnected,
    Connecting,
    Online,
    Retrying,
    Conflict,
    Incompatible,
    CredentialInvalid,
    Failed,
}
