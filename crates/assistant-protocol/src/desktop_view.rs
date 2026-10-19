//! 桌面查看的应用意图；SDP、ICE 与媒体反馈仍归 Host/客户端私有传输实现。
use crate::{ClientCompatibility, SecretValue};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 每次打开固定目标设备与登录；不持久化凭据，不复用旧连接的访问编号。
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DesktopViewOpenRequest {
    pub device_id: String,
    /// 空值选择主屏；每次切屏重新创建查看访问。
    pub display_id: Option<String>,
    #[ts(type = "string | null")]
    pub bearer: Option<SecretValue>,
    pub login_context: Option<String>,
    pub client_compatibility: Option<ClientCompatibility>,
}

/// 目标设备当前可查看显示器，尺寸为物理像素；不包含窗口或桌面内容。
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DesktopDisplay {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// 单次输入绑定已有查看、控制和显示器；序号仅在本控制期间有效，不持久化或重放。
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DesktopInputFrame {
    pub view_id: String,
    pub control_id: String,
    pub display_id: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub input: DesktopInput,
}

/// 物理键与提交文字分开；文本按字符边界分段，结束前不会注入。
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum DesktopInput {
    Heartbeat,
    Release,
    Move {
        x: f64,
        y: f64,
    },
    Button {
        button: u8,
        down: bool,
        clicks: u8,
    },
    Key {
        code: String,
        down: bool,
        repeat: bool,
    },
    Scroll {
        x: i32,
        y: i32,
    },
    TextStart,
    TextChunk {
        text: String,
    },
    TextEnd,
}
