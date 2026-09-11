//! 客户端建立 HTTP 连接前可查询的 Host 状态与能力投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 受认证的启动投影；版本未知用 None，安全错误码不包含配置正文或磁盘路径。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeHostHealth {
    pub status: RuntimeHostHealthStatus,
    pub stage: Option<RuntimeHostStartupStage>,
    pub database_version: Option<String>,
    /// 只投影经过规范校验的数据库要求；读取失败或旧端未提供时未知。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_compatible_host_version: Option<String>,
    pub target_version: String,
    pub error: Option<RuntimeHostStartupError>,
}

/// `/health` 的稳定就绪状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostHealthStatus {
    Starting,
    Ready,
    Unavailable,
}

/// 当前正在执行的初始化阶段，不表示百分比或预计耗时。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostStartupStage {
    DatabaseCheck,
    DatabaseBackup,
    DatabaseMigration,
    Configuration,
    Recovery,
}

/// 失败后只查询诊断；修复并重新启动 Host 才重新尝试初始化。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostStartupError {
    DatabaseUnavailable,
    DatabaseNewer,
    DatabaseHostTooOld,
    DatabaseUnsafeJournal,
    MigrationFailed,
    BackupFailed,
    ConfigurationInvalid,
    InitializationFailed,
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
    HostAccess,
    WebLogin,
    UserTerminals,
    StartupDiagnostics,
}

/// 当前 Host 实例公开给客户端的传输能力，不包含地址、Token 或业务状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RuntimeHostCapabilities {
    pub min_compatible_version: String,
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
            stage: None,
            database_version: None,
            min_compatible_host_version: None,
            target_version: "0.25.0".into(),
            error: None,
        };
        assert_eq!(
            serde_json::to_string(&health).expect("health JSON"),
            r#"{"status":"ready","stage":null,"database_version":null,"target_version":"0.25.0","error":null}"#
        );

        let capabilities = RuntimeHostCapabilities {
            min_compatible_version: "0.25.2".into(),
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
