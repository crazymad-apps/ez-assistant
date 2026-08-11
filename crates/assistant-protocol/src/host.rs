//! 客户端建立 HTTP 连接前可查询的 Host 状态与能力投影。

use serde::{Deserialize, Serialize};

/// Host 已完成 Runtime 恢复并可以接受已授权请求。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeHostHealth {
    pub status: RuntimeHostHealthStatus,
}

/// `/health` 的稳定就绪状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostHealthStatus {
    Ready,
}

/// 当前 Host 实例公开给客户端的传输能力，不包含地址、Token 或业务状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeHostCapabilities {
    pub protocol_version: u32,
    pub runtime_version: String,
    pub max_command_bytes: u64,
    pub max_attachment_bytes: Option<u64>,
    pub sse: bool,
    pub streaming_upload: bool,
    pub private_web_demo: bool,
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
            private_web_demo: true,
        };
        let json = serde_json::to_string(&capabilities).expect("capabilities JSON");
        assert!(!json.contains("token"));
        assert_eq!(
            serde_json::from_str::<RuntimeHostCapabilities>(&json).expect("decode capabilities"),
            capabilities
        );
    }
}
