//! 用户终端连接的控制载荷；字节 I/O 使用 WebSocket 二进制帧，不进入会话事件或持久化。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{SecretValue, SessionId, SessionResourceLocator, WorkspaceId};

/// 启动目录由 Host 根据已登记来源重新解析，客户端不指定 Shell 或裸目录。
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserTerminalSource {
    Session {
        session_id: SessionId,
        locator: SessionResourceLocator,
    },
    Workspace {
        workspace_id: WorkspaceId,
    },
}

/// 字符网格尺寸，Host 校验 cols 2—1000、rows 1—500。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UserTerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// 每个连接只能 Open 一次；ACK 只确认当前一个输出块，不累积未来额度。
#[derive(Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserTerminalControl {
    Open {
        #[ts(type = "string | null")]
        bearer: Option<SecretValue>,
        source: UserTerminalSource,
        size: UserTerminalSize,
    },
    Resize {
        size: UserTerminalSize,
    },
    Ack,
    Close,
}

/// 只对所属连接可见。Closed 表示 PTY 已完成清理，Error 不假称回收成功。
#[derive(Debug, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserTerminalNotice {
    Created {
        terminal_id: String,
        directory_name: String,
    },
    Exited {
        code: u32,
    },
    Error {
        message: String,
    },
    /// 一块输入已写入 PTY 后才允许客户端继续发送，避免大粘贴耗尽队列。
    InputAck,
    Closed,
}
