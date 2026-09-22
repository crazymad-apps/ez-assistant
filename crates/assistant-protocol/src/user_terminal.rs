//! 用户终端连接的控制载荷；字节 I/O 使用 WebSocket 二进制帧，不进入会话事件或持久化。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{SecretValue, SessionId, SessionResourceLocator, WorkspaceId};

/// 固定 Shell 身份；客户端不得提交任意解释器路径或启动参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    #[serde(rename = "windows_powershell_51")]
    WindowsPowershell51,
    Cmd,
    #[serde(rename = "powershell_7")]
    Powershell7,
    GitBash,
    PosixSh,
}

/// 当前 Host 探测到的固定 Shell；不可用项保留身份及可解释原因。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ShellCatalogEntry {
    pub kind: ShellKind,
    pub available: bool,
    pub reason: Option<String>,
}

/// Host 解释器目录与新 Session 默认值；None 表示平台默认，既有 Session 不随设置变化。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AgentShellSettings {
    pub default_agent_shell: Option<ShellKind>,
    pub catalog: Vec<ShellCatalogEntry>,
}

/// 启动目录由 Host 根据已登记来源重新解析，客户端不指定裸目录。
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
        client_compatibility: Option<crate::ClientCompatibility>,
        #[ts(type = "string | null")]
        bearer: Option<SecretValue>,
        /// Cookie 页面期望的登录上下文；不可作为凭据授权。
        #[serde(default)]
        login_context: Option<String>,
        source: UserTerminalSource,
        size: UserTerminalSize,
        /// 未提供时使用 Host 平台默认；显式不可用的 Shell 不自动回退。
        #[serde(default)]
        shell: Option<ShellKind>,
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
    CompatibilityError {
        error: crate::RuntimeCompatibilityError,
    },
    Created {
        terminal_id: String,
        directory_name: String,
        /// 本次启动确认的固定类型；Unix 默认登录 Shell 不伪装成 /bin/sh。
        shell: Option<ShellKind>,
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
