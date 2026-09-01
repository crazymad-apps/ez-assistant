//! Desktop 管理设备接入所需的稳定意图和安全投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DeviceId, OutputPreferenceSnapshot, SecretValue};

/// Host 与设备协商后可对 Desktop 展示的有效能力集合。
///
/// 音频能力已经与 Host 当前 ASR/TTS 可用性取交集，不等同于设备自报能力。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceCapabilitiesSnapshot {
    pub input_text: bool,
    pub input_pcm16_16k_mono: bool,
    pub output_text: bool,
    pub output_pcm16_16k_mono: bool,
    pub playback_cancel: bool,
    pub display_status: bool,
    pub display_transcript: bool,
}

/// Desktop 可见的稳定设备登记生命周期，不表达在线状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum DeviceLifecycleSnapshot {
    Paired,
    Revoked,
}

/// 一台已配对设备当前在线连接的易失投影。
///
/// 设备离线时整个字段从设备摘要中消失，不把离线写回持久设备记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceConnectionSnapshot {
    pub connected_at_ms: i64,
    pub capabilities: DeviceCapabilitiesSnapshot,
    pub output_preference: OutputPreferenceSnapshot,
}

/// Desktop 管理页使用的设备摘要，由 Runtime 登记事实和 Host 在线状态组合而成。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceSummarySnapshot {
    pub device_id: DeviceId,
    pub display_name: String,
    pub lifecycle: DeviceLifecycleSnapshot,
    pub paired_at_ms: i64,
    pub updated_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connection: Option<DeviceConnectionSnapshot>,
}

/// 已连接但尚未完成用户确认的配对候选。
///
/// 候选及尝试次数只存在于当前 Host 进程，不是持久设备身份。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PendingDevicePairingSnapshot {
    pub pairing_request_id: String,
    pub display_name: String,
    pub capabilities: DeviceCapabilitiesSnapshot,
    pub expires_at_ms: i64,
    pub remaining_attempts: u8,
}

/// 当前允许附近设备发起配对的限时窗口。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DevicePairingWindowSnapshot {
    pub expires_at_ms: i64,
}

/// 单项 Host 语音能力的运行状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SpeechServiceStatusSnapshot {
    /// 配置有效且当前可接受请求。
    Ready,
    /// 子系统仍存活，但最近请求或任务出现运行时故障。
    Degraded,
    /// 未配置、配置无效或能力无法装配。
    #[default]
    Unavailable,
}

/// Host 当前 ASR 与 TTS 的独立能力投影；任一方失败不会连带关闭另一方。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceSpeechServicesSnapshot {
    pub asr: SpeechServiceStatusSnapshot,
    pub tts: SpeechServiceStatusSnapshot,
}

/// Desktop 读取的 Device Gateway 完整权威快照。
///
/// Gateway 事件只负责通知失效；客户端收到事件后应重新读取本快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceGatewaySnapshot {
    pub enabled: bool,
    pub available: bool,
    pub installation_id: String,
    pub certificate_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pairing_window: Option<DevicePairingWindowSnapshot>,
    pub pending_pairings: Vec<PendingDevicePairingSnapshot>,
    pub devices: Vec<DeviceSummarySnapshot>,
    #[serde(default)]
    pub speech_services: DeviceSpeechServicesSnapshot,
}

/// Host Device Gateway 的可丢弃失效通知；完整状态始终重新读取 Gateway 快照。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceGatewayEvent {
    Changed,
}

/// 查询 Device Gateway 完整快照的空请求。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetDeviceGatewaySnapshotRequest {}

/// 启用或关闭智能终端接入能力的用户意图。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetDeviceAccessEnabledRequest {
    pub enabled: bool,
}

/// 打开限时配对窗口的交互命令参数。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct OpenDevicePairingWindowRequest {}

/// 提前关闭当前配对窗口的交互命令参数。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CloseDevicePairingWindowRequest {}

/// 用户用候选设备展示的配对码确认一次配对请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConfirmDevicePairingRequest {
    pub pairing_request_id: String,
    #[ts(type = "string")]
    pub pairing_code: SecretValue,
    pub display_name: Option<String>,
}

/// 修改已配对设备显示名称的请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RenameDeviceRequest {
    pub device_id: DeviceId,
    pub display_name: String,
}

/// 吊销设备持久身份并断开其当前连接的请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RevokeDeviceRequest {
    pub device_id: DeviceId,
}

/// Desktop 发往 Host 的 Device Gateway 管理命令集合。
///
/// 这些是 Host 级交互意图，不属于 Runtime Session 命令。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum DeviceGatewayCommand {
    GetSnapshot(GetDeviceGatewaySnapshotRequest),
    SetAccessEnabled(SetDeviceAccessEnabledRequest),
    OpenPairingWindow(OpenDevicePairingWindowRequest),
    ClosePairingWindow(CloseDevicePairingWindowRequest),
    ConfirmPairing(ConfirmDevicePairingRequest),
    RenameDevice(RenameDeviceRequest),
    RevokeDevice(RevokeDeviceRequest),
}

/// Gateway 写操作完成后返回的最新组合快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceGatewayMutationResult {
    pub snapshot: DeviceGatewaySnapshot,
}

/// 与 [`DeviceGatewayCommand`] 一一对应的 Host 命令结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum DeviceGatewayCommandResult {
    GetSnapshot(DeviceGatewaySnapshot),
    SetAccessEnabled(DeviceGatewayMutationResult),
    OpenPairingWindow(DeviceGatewayMutationResult),
    ClosePairingWindow(DeviceGatewayMutationResult),
    ConfirmPairing(DeviceGatewayMutationResult),
    RenameDevice(DeviceGatewayMutationResult),
    RevokeDevice(DeviceGatewayMutationResult),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_gateway_command_round_trips_with_stable_tagging() {
        let command = DeviceGatewayCommand::ConfirmPairing(ConfirmDevicePairingRequest {
            pairing_request_id: "pairing-1".to_owned(),
            pairing_code: SecretValue::new("123456".to_owned()),
            display_name: Some("客厅终端".to_owned()),
        });
        let value = serde_json::to_value(&command).expect("serialize command");
        assert_eq!(value["type"], "confirm_pairing");
        let decoded: DeviceGatewayCommand =
            serde_json::from_value(value).expect("deserialize command");
        assert_eq!(decoded, command);
    }

    #[test]
    fn pairing_code_is_redacted_from_debug_output() {
        let command = DeviceGatewayCommand::ConfirmPairing(ConfirmDevicePairingRequest {
            pairing_request_id: "pairing-1".to_owned(),
            pairing_code: SecretValue::new("123456".to_owned()),
            display_name: None,
        });
        let debug = format!("{command:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("123456"));
    }

    #[test]
    fn gateway_event_and_speech_status_have_stable_wire_values() {
        assert_eq!(
            serde_json::to_value(DeviceGatewayEvent::Changed).expect("event"),
            serde_json::json!({ "type": "changed" })
        );
        assert_eq!(
            serde_json::to_value(SpeechServiceStatusSnapshot::Unavailable).expect("status"),
            serde_json::json!("unavailable")
        );
    }
}
