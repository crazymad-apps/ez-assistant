//! 客户端建立 HTTP 连接前可查询的 Host 状态与能力投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Host 已完成 Runtime 恢复并可以接受已授权请求。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeHostHealth {
    pub status: RuntimeHostHealthStatus,
}

/// `/health` 的稳定就绪状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostHealthStatus {
    Ready,
}

/// Host 可以逐项声明的产品能力；Desktop 只检查当前页面实际依赖的项目。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostFeature {
    EventEnvelopes,
    ApplicationSnapshot,
    SessionView,
    ChildTaskView,
    ConversationPaging,
    ToolDetail,
    QueueControl,
    ApprovalQueue,
    SessionManagement,
    SessionMaterialization,
    SessionResourceFiles,
}

/// 当前 Host 实例公开给客户端的传输能力，不包含地址、Token 或业务状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeHostCapabilities {
    pub protocol_version: u32,
    pub runtime_version: String,
    pub max_command_bytes: u64,
    pub max_attachment_bytes: Option<u64>,
    pub sse: bool,
    pub streaming_upload: bool,
    /// Additive 产品能力；空列表表示 Host 尚未启用正式 Desktop 产品投影。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<RuntimeHostFeature>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_health_and_capabilities_round_trip_without_secrets() {
        let health = RuntimeHostHealth {
            status: RuntimeHostHealthStatus::Ready,
        };
        assert_eq!(
            serde_json::to_string(&health).expect("health JSON"),
            r#"{"status":"ready"}"#
        );

        let capabilities = RuntimeHostCapabilities {
            protocol_version: 1,
            runtime_version: "0.1.0".to_owned(),
            max_command_bytes: 1024 * 1024,
            max_attachment_bytes: Some(1024 * 1024 * 1024),
            sse: true,
            streaming_upload: true,
            features: Vec::new(),
        };
        let json = serde_json::to_string(&capabilities).expect("capabilities JSON");
        assert!(!json.contains("token"));
        assert!(!json.contains("features"));
        assert_eq!(
            serde_json::from_str::<RuntimeHostCapabilities>(&json).expect("decode capabilities"),
            capabilities
        );
        assert_eq!(
            serde_json::to_string(&RuntimeHostFeature::SessionManagement).expect("feature JSON"),
            r#""session_management""#
        );
    }
}
