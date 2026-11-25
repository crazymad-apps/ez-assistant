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

/// 配对确认方式；默认保持 6 位数字 SPAKE2 口令，同时支持桌面端一键确认免码配对。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum DevicePairingModeSnapshot {
    #[default]
    Spake2,
    DirectConfirm,
}

/// 在线设备在连接握手时声明的 Web/HTTP 控制台安全元数据。
///
/// 主机名严格由 Host 根据当前连接的真实对端 IP 绑定，`direct_url` 仅在声明 `ui_path` 时生成。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DeviceConsoleSnapshot {
    pub api_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ui_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub direct_url: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub peer_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_server_keys: Vec<crate::McpServerKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub console: Option<DeviceConsoleSnapshot>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub speech_relay: bool,
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
    #[serde(default)]
    pub mode: DevicePairingModeSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub peer_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub key_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub client_version: Option<String>,
}

/// 局域网中通过 `_ez-device._tcp.local.` mDNS 发现的待配对智能终端。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct DiscoveredDeviceSnapshot {
    pub discovery_id: String,
    pub display_name: String,
    pub model: String,
    pub peer_address: String,
    pub control_port: u16,
    pub key_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mac_address: Option<String>,
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
    #[serde(default)]
    pub discovered_devices: Vec<DiscoveredDeviceSnapshot>,
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

/// 用户在桌面端确认一次配对请求（SPAKE2 携带 6 位配对码；免码直连不传配对码）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConfirmDevicePairingRequest {
    pub pairing_request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string | null")]
    pub pairing_code: Option<SecretValue>,
    pub display_name: Option<String>,
}

/// 用户在桌面端拒绝一次待确认的配对请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RejectDevicePairingRequest {
    pub pairing_request_id: String,
}

/// 一键连接局域网中发现的智能终端：自动开启接入、授权目标密钥指纹并向终端下发连接指令。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ConnectDiscoveredDeviceRequest {
    pub discovery_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
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

/// Host 语音服务支持的 Provider 类型。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SpeechProviderKind {
    #[default]
    Dashscope,
    Deepseek,
    Siliconflow,
    Zhipu,
    Minimax,
    Openai,
    Groq,
}

/// Host ASR 配置的安全读取投影（凭据仅返回是否已配置与脱敏掩码）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct AsrConfigurationSnapshot {
    pub enabled: bool,
    pub provider: SpeechProviderKind,
    pub endpoint: String,
    pub model: String,
    pub timeout_ms: u64,
    pub credential_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub credential_masked: Option<String>,
}

/// Host TTS 配置的安全读取投影（凭据仅返回是否已配置与脱敏掩码）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TtsConfigurationSnapshot {
    pub enabled: bool,
    pub provider: SpeechProviderKind,
    pub endpoint: String,
    pub model: String,
    pub voice: String,
    pub timeout_ms: u64,
    pub credential_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub credential_masked: Option<String>,
}

/// Host 语音配置与当前服务健康快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SpeechConfigurationSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
    pub asr: AsrConfigurationSnapshot,
    pub tts: TtsConfigurationSnapshot,
    pub status: DeviceSpeechServicesSnapshot,
}

/// 读取当前 Host 语音配置（ASR / TTS）的请求。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct GetSpeechConfigurationRequest {}

/// 更新 Host ASR 配置的请求参数；凭据留空且 `clear_credential = false` 时保留既有凭据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateAsrConfigurationInput {
    pub enabled: bool,
    pub provider: SpeechProviderKind,
    pub endpoint: String,
    pub model: String,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string | null")]
    pub credential: Option<SecretValue>,
    #[serde(default)]
    pub clear_credential: bool,
}

/// 更新 Host TTS 配置的请求参数；凭据留空且 `clear_credential = false` 时保留既有凭据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateTtsConfigurationInput {
    pub enabled: bool,
    pub provider: SpeechProviderKind,
    pub endpoint: String,
    pub model: String,
    pub voice: String,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string | null")]
    pub credential: Option<SecretValue>,
    #[serde(default)]
    pub clear_credential: bool,
    #[serde(default)]
    pub reuse_asr_credential: bool,
}

/// 保存 Host 语音配置并立即热重载 `SpeechService` 的请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateSpeechConfigurationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_revision: Option<String>,
    pub asr: UpdateAsrConfigurationInput,
    pub tts: UpdateTtsConfigurationInput,
}

/// 更新语音配置后返回的最新脱敏配置与 Gateway 状态快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateSpeechConfigurationResult {
    pub configuration: SpeechConfigurationSnapshot,
    pub snapshot: DeviceGatewaySnapshot,
}

/// 使用草稿或已保存凭据直接验证 ASR 转写效果的请求（音频为 Base64 WAV）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestAsrConfigurationRequest {
    pub asr: UpdateAsrConfigurationInput,
    pub audio_wav_base64: String,
}

/// ASR 测试结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestAsrConfigurationResult {
    pub transcript: String,
    pub elapsed_ms: u64,
}

/// 使用草稿或已保存凭据直接验证 TTS 合成与试听效果的请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestTtsConfigurationRequest {
    pub tts: UpdateTtsConfigurationInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string | null")]
    pub fallback_asr_credential: Option<SecretValue>,
    pub text: String,
}

/// TTS 测试结果（返回可直接在桌面端播放的 16 kHz Mono Base64 WAV）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct TestTtsConfigurationResult {
    pub audio_wav_base64: String,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
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
    RejectPairing(RejectDevicePairingRequest),
    ConnectDiscoveredDevice(ConnectDiscoveredDeviceRequest),
    RenameDevice(RenameDeviceRequest),
    RevokeDevice(RevokeDeviceRequest),
    GetSpeechConfiguration(GetSpeechConfigurationRequest),
    UpdateSpeechConfiguration(UpdateSpeechConfigurationRequest),
    TestAsrConfiguration(TestAsrConfigurationRequest),
    TestTtsConfiguration(TestTtsConfigurationRequest),
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
    RejectPairing(DeviceGatewayMutationResult),
    ConnectDiscoveredDevice(DeviceGatewayMutationResult),
    RenameDevice(DeviceGatewayMutationResult),
    RevokeDevice(DeviceGatewayMutationResult),
    GetSpeechConfiguration(SpeechConfigurationSnapshot),
    UpdateSpeechConfiguration(UpdateSpeechConfigurationResult),
    TestAsrConfiguration(TestAsrConfigurationResult),
    TestTtsConfiguration(TestTtsConfigurationResult),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_gateway_command_round_trips_with_stable_tagging() {
        let command = DeviceGatewayCommand::ConfirmPairing(ConfirmDevicePairingRequest {
            pairing_request_id: "pairing-1".to_owned(),
            pairing_code: Some(SecretValue::new("123456".to_owned())),
            display_name: Some("客厅终端".to_owned()),
        });
        let value = serde_json::to_value(&command).expect("serialize command");
        assert_eq!(value["type"], "confirm_pairing");
        let decoded: DeviceGatewayCommand =
            serde_json::from_value(value).expect("deserialize command");
        assert_eq!(decoded, command);

        let direct = DeviceGatewayCommand::ConfirmPairing(ConfirmDevicePairingRequest {
            pairing_request_id: "pairing-2".to_owned(),
            pairing_code: None,
            display_name: None,
        });
        let direct_value = serde_json::to_value(&direct).expect("serialize direct confirm");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(direct_value)
                .expect("deserialize direct confirm"),
            direct
        );

        let reject = DeviceGatewayCommand::RejectPairing(RejectDevicePairingRequest {
            pairing_request_id: "pairing-3".to_owned(),
        });
        let reject_value = serde_json::to_value(&reject).expect("serialize reject");
        assert_eq!(reject_value["type"], "reject_pairing");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(reject_value)
                .expect("deserialize reject"),
            reject
        );
    }

    #[test]
    fn pairing_code_is_redacted_from_debug_output() {
        let command = DeviceGatewayCommand::ConfirmPairing(ConfirmDevicePairingRequest {
            pairing_request_id: "pairing-1".to_owned(),
            pairing_code: Some(SecretValue::new("123456".to_owned())),
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

    #[test]
    fn speech_configuration_commands_round_trip_and_redact_credentials() {
        let command =
            DeviceGatewayCommand::UpdateSpeechConfiguration(UpdateSpeechConfigurationRequest {
                expected_revision: Some("rev-1".to_owned()),
                asr: UpdateAsrConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Openai,
                    endpoint: "https://api.openai.com/v1".to_owned(),
                    model: "whisper-1".to_owned(),
                    timeout_ms: 30_000,
                    credential: Some(SecretValue::new("sk-secret-asr-token".to_owned())),
                    clear_credential: false,
                },
                tts: UpdateTtsConfigurationInput {
                    enabled: true,
                    provider: SpeechProviderKind::Dashscope,
                    endpoint: "https://dashscope.aliyuncs.com".to_owned(),
                    model: "cosyvoice-v2".to_owned(),
                    voice: "longxiaochun_v2".to_owned(),
                    timeout_ms: 30_000,
                    credential: Some(SecretValue::new("sk-secret-tts-token".to_owned())),
                    clear_credential: false,
                    reuse_asr_credential: false,
                },
            });
        let debug = format!("{command:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-secret-asr-token"));
        assert!(!debug.contains("sk-secret-tts-token"));

        let wire = serde_json::to_value(&command).expect("serialize update speech");
        assert_eq!(wire["type"], "update_speech_configuration");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(wire)
                .expect("deserialize update speech"),
            command
        );

        let get_cmd =
            DeviceGatewayCommand::GetSpeechConfiguration(GetSpeechConfigurationRequest {});
        let get_wire = serde_json::to_value(&get_cmd).expect("serialize get speech");
        assert_eq!(get_wire["type"], "get_speech_configuration");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(get_wire)
                .expect("deserialize get speech"),
            get_cmd
        );

        for (kind, wire_name) in [
            (SpeechProviderKind::Dashscope, "dashscope"),
            (SpeechProviderKind::Deepseek, "deepseek"),
            (SpeechProviderKind::Siliconflow, "siliconflow"),
            (SpeechProviderKind::Zhipu, "zhipu"),
            (SpeechProviderKind::Minimax, "minimax"),
            (SpeechProviderKind::Openai, "openai"),
            (SpeechProviderKind::Groq, "groq"),
        ] {
            let encoded = serde_json::to_value(kind).expect("serialize speech provider");
            assert_eq!(encoded, serde_json::json!(wire_name));
            assert_eq!(
                serde_json::from_value::<SpeechProviderKind>(encoded).expect("decode provider"),
                kind
            );
        }

        let test_asr = DeviceGatewayCommand::TestAsrConfiguration(TestAsrConfigurationRequest {
            asr: UpdateAsrConfigurationInput {
                enabled: false,
                provider: SpeechProviderKind::Siliconflow,
                endpoint: "https://api.siliconflow.cn/v1".to_owned(),
                model: "FunAudioLLM/SenseVoiceSmall".to_owned(),
                timeout_ms: 30_000,
                credential: Some(SecretValue::new("sk-asr-draft-secret".to_owned())),
                clear_credential: false,
            },
            audio_wav_base64: "UklGRg==".to_owned(),
        });
        let asr_debug = format!("{test_asr:?}");
        assert!(asr_debug.contains("<redacted>"));
        assert!(!asr_debug.contains("sk-asr-draft-secret"));
        let asr_wire = serde_json::to_value(&test_asr).expect("serialize test asr");
        assert_eq!(asr_wire["type"], "test_asr_configuration");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(asr_wire).expect("deserialize test asr"),
            test_asr
        );

        let test_tts = DeviceGatewayCommand::TestTtsConfiguration(TestTtsConfigurationRequest {
            tts: UpdateTtsConfigurationInput {
                enabled: false,
                provider: SpeechProviderKind::Minimax,
                endpoint: "https://api.minimaxi.com/v1/t2a_v2".to_owned(),
                model: "speech-2.8-turbo".to_owned(),
                voice: "female-shaonv".to_owned(),
                timeout_ms: 30_000,
                credential: None,
                clear_credential: false,
                reuse_asr_credential: true,
            },
            fallback_asr_credential: Some(SecretValue::new("sk-fallback-tts-secret".to_owned())),
            text: "你好，这是语音合成测试。".to_owned(),
        });
        let tts_debug = format!("{test_tts:?}");
        assert!(tts_debug.contains("<redacted>"));
        assert!(!tts_debug.contains("sk-fallback-tts-secret"));
        let tts_wire = serde_json::to_value(&test_tts).expect("serialize test tts");
        assert_eq!(tts_wire["type"], "test_tts_configuration");
        assert_eq!(
            serde_json::from_value::<DeviceGatewayCommand>(tts_wire).expect("deserialize test tts"),
            test_tts
        );
    }
}
