//! 用户级设备目录与访问意图；传输凭据仅由 Host 使用。
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum HostNetworkCapability {
    Ordinary,
    DesktopView,
    DesktopControl,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum HostNetworkCommand {
    ListDevices {
        offset: u32,
    },
    OpenAccess {
        device_id: String,
        capability: HostNetworkCapability,
    },
    GetAccess {
        access_id: String,
    },
    CloseAccess {
        access_id: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct HostNetworkDevice {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub enabled: bool,
    pub online: bool,
    pub desktop_view: bool,
    pub desktop_control: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum HostNetworkResult {
    Devices {
        /// 当前直连 Host 的登记编号；未登记时为空，不授予登记管理权限。
        self_device_id: Option<String>,
        items: Vec<HostNetworkDevice>,
        next_offset: Option<u32>,
    },
    Access {
        access_id: String,
        device_id: String,
        capability: HostNetworkCapability,
        active: bool,
    },
    Closed,
}
