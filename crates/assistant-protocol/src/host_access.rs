//! Host 访问设置与普通登录契约；不进入 Runtime 会话或设备配对领域。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::SecretValue;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum HostAccessScheme {
    #[default]
    Http,
    Https,
}

/// 公开给已登录所有者的监听配置；证书字段是 Host 路径，不含文件内容。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(default, deny_unknown_fields)]
pub struct HostAccessConfiguration {
    pub remote_enabled: bool,
    pub scheme: HostAccessScheme,
    pub port: u16,
    /// 可选域名白名单；IP 地址和本机 localhost 不需要重复登记。
    pub server_names: Vec<String>,
    pub tls_certificate: Option<String>,
    pub tls_private_key: Option<String>,
}

impl Default for HostAccessConfiguration {
    fn default() -> Self {
        Self {
            remote_enabled: false,
            scheme: HostAccessScheme::Http,
            port: 7240,
            server_names: Vec::new(),
            tls_certificate: None,
            tls_private_key: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum HostListenerState {
    Closed,
    Listening,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct HostAccessStatus {
    pub revision: Option<String>,
    pub password_configured: bool,
    pub configuration: HostAccessConfiguration,
    pub listener_state: HostListenerState,
    /// 保存的端口、协议或证书与当前监听不一致，需要显式重启 Host。
    pub restart_required: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum HostAccessCommand {
    GetStatus,
    SetPassword {
        expected_revision: Option<String>,
        #[ts(type = "string")]
        password: SecretValue,
    },
    Configure {
        expected_revision: Option<String>,
        configuration: HostAccessConfiguration,
    },
}

/// Web 只接收 Cookie；原生客户端显式选择 token 响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostLoginRequest {
    Password {
        #[ts(type = "string")]
        password: SecretValue,
        native: bool,
    },
    Token {
        #[ts(type = "string")]
        token: SecretValue,
    },
    /// 已认证的 Desktop 创建独立浏览器登录，不把原生 bootstrap 传给页面。
    Desktop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct HostLoginResult {
    #[ts(type = "string | null")]
    pub token: Option<SecretValue>,
    pub expires_at_ms: u64,
    pub instance_id: String,
}
