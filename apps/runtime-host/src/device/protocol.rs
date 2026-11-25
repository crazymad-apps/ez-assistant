//! Device wire 1.0 的有界 JSON envelope、能力与控制 payload。

use std::collections::{BTreeMap, HashSet};

use assistant_protocol::{
    DeviceCapabilitiesSnapshot, DeviceSpeechServicesSnapshot, InputId, OutputPreferenceSnapshot,
    RunId, RunStatus, SubmitInputResult,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

pub(super) const PROTOCOL_MAJOR: u16 = 1;
pub(super) const PROTOCOL_MINOR: u16 = 0;
pub(super) const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
pub(super) const PCM_HEADER_BYTES: usize = 16;
pub(super) const PCM_PAYLOAD_BYTES: usize = 640;
const PCM_FRAME_BYTES: usize = PCM_HEADER_BYTES + PCM_PAYLOAD_BYTES;
pub(super) const PCM_FRAME_KIND_VOICE_UPLINK: u8 = 1;
pub(super) const PCM_FRAME_KIND_RELAY_UPLINK: u8 = 3;
pub(super) const PCM_FRAME_KIND_RELAY_DOWNLINK: u8 = 4;
pub(super) const RELAY_UPLINK_MAX_PAYLOAD_BYTES: usize = 32 * 1024;
pub(super) const RELAY_DOWNLINK_CHUNK_BYTES: usize = 16_000;
const MAX_MESSAGE_ID_BYTES: usize = 128;
const MAX_REPLAY_WINDOW: usize = 256;

/// Device wire 控制消息的统一 JSON 外壳。
///
/// `message_id` 只在当前连接内参与重放检测，不是 Runtime Input 或业务幂等键。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub(super) protocol_major: u16,
    pub(super) protocol_minor: u16,
    pub(super) message_id: String,
    #[serde(rename = "type")]
    pub(super) message_type: String,
    pub(super) payload: Value,
}

/// 当前连接最近控制消息 ID 的有界重放窗口。
///
/// 它防止同一连接重复执行控制动作；跨连接业务去重由 `client_input_id` 负责。
#[derive(Default)]
pub(super) struct MessageReplayWindow {
    ids: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl MessageReplayWindow {
    pub(super) fn accept(&mut self, message_id: &str) -> Result<(), ProtocolError> {
        if !self.ids.insert(message_id.to_owned()) {
            return Err(ProtocolError::DuplicateMessageId);
        }
        self.order.push_back(message_id.to_owned());
        if self.order.len() > MAX_REPLAY_WINDOW
            && let Some(expired) = self.order.pop_front()
        {
            self.ids.remove(&expired);
        }
        Ok(())
    }
}

impl Envelope {
    pub(super) fn decode(text: &str) -> Result<Self, ProtocolError> {
        if text.len() > MAX_CONTROL_MESSAGE_BYTES {
            return Err(ProtocolError::MessageTooLarge);
        }
        let envelope: Self = serde_json::from_str(text).map_err(|_| ProtocolError::InvalidJson)?;
        if envelope.protocol_major != PROTOCOL_MAJOR {
            return Err(ProtocolError::UnsupportedProtocol);
        }
        if envelope.message_id.trim().is_empty()
            || envelope.message_id.len() > MAX_MESSAGE_ID_BYTES
            || envelope.message_type.trim().is_empty()
            || envelope.message_type.len() > 64
        {
            return Err(ProtocolError::InvalidEnvelope);
        }
        Ok(envelope)
    }

    pub(super) fn new<T: Serialize>(
        message_id: String,
        message_type: &str,
        payload: &T,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            message_id,
            message_type: message_type.to_owned(),
            payload: serde_json::to_value(payload).map_err(|_| ProtocolError::InvalidPayload)?,
        })
    }

    pub(super) fn payload<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        serde_json::from_value(self.payload.clone()).map_err(|_| ProtocolError::InvalidPayload)
    }

    pub(super) fn encode(&self) -> Result<String, ProtocolError> {
        let encoded = serde_json::to_string(self).map_err(|_| ProtocolError::InvalidPayload)?;
        if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
            return Err(ProtocolError::MessageTooLarge);
        }
        Ok(encoded)
    }
}

/// 设备在 `pairing_hello` 中声明的配对模式；缺省保持旧版 SPAKE2。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WirePairingMode {
    #[default]
    Spake2,
    DirectConfirm,
}

/// 设备发起配对时提交的临时候选身份及 PAKE 或 Ed25519 公钥。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingHello {
    pub(super) pairing_request_id: String,
    pub(super) display_name: String,
    pub(super) device_nonce: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    #[serde(default)]
    pub(super) pairing_mode: WirePairingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pake_share: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) public_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) client_version: Option<String>,
}

/// 桌面端点击确认免码配对后，Host 下发的挑战与分配设备 ID。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingDirectChallenge {
    pub(super) pairing_request_id: String,
    pub(super) host_nonce: String,
    pub(super) device_id: String,
}

/// 免码配对中设备使用其长期私钥对绑定 transcript 作出的最终签名确认。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingDirectCommit {
    pub(super) pairing_request_id: String,
    pub(super) device_id: String,
    pub(super) signature: String,
}

/// 设备声明的反向 MCP HTTP 传输协议。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeviceMcpScheme {
    #[default]
    Http,
    Https,
}

/// 已配对设备在认证 `hello` 中声明的自托管 Streamable HTTP MCP 端点元数据（不含 Host，由连接对端 IP 绑定）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceMcpServerDeclaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) id: Option<String>,
    pub(super) display_name: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) scheme: DeviceMcpScheme,
    pub(super) port: u16,
    pub(super) path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) headers: BTreeMap<String, String>,
}

/// Host 已接管配对候选并等待 Desktop 确认的通知。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingPending {
    pub(super) pairing_request_id: String,
    pub(super) expires_at_ms: i64,
}

/// Desktop 确认配对码后，Host 返回的 PAKE 份额及握手证明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingPake {
    pub(super) pairing_request_id: String,
    pub(super) host_nonce: String,
    pub(super) pake_share: String,
    pub(super) confirmation_mac: String,
}

/// 设备对 Host PAKE 结果的确认；通过后才能进入长期密钥绑定阶段。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingConfirmation {
    pub(super) pairing_request_id: String,
    pub(super) confirmation_mac: String,
}

/// 设备提交长期公钥及其对本次临时握手的绑定证明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingBind {
    pub(super) pairing_request_id: String,
    pub(super) public_key: String,
    pub(super) signature: String,
    pub(super) binding_mac: String,
}

/// Host 接受长期公钥但尚未持久登记设备时返回的绑定确认。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingBindAck {
    pub(super) pairing_request_id: String,
    pub(super) device_id: String,
    pub(super) host_proof: String,
}

/// 设备对分配身份的最终提交；验证通过后 Host 才写入稳定配对记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingCommit {
    pub(super) pairing_request_id: String,
    pub(super) device_id: String,
    pub(super) signature: String,
    pub(super) binding_mac: String,
}

/// 配对记录已经可靠持久化后的最终完成通知。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PairingComplete {
    pub(super) pairing_request_id: String,
    pub(super) device_id: String,
    pub(super) display_name: String,
}

/// 已配对设备连接后由 Host 发出的短期认证挑战。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthChallenge {
    pub(super) connection_id: String,
    pub(super) nonce: String,
    pub(super) server_time_ms: i64,
}

fn default_console_api_prefix() -> String {
    "/api/v1".to_owned()
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 已配对设备声明的控制面板元信息（不含 Host，由连接对端 IP 绑定）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceConsoleDeclaration {
    #[serde(default)]
    pub(super) scheme: DeviceMcpScheme,
    pub(super) port: u16,
    #[serde(default = "default_console_api_prefix")]
    pub(super) api_prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ui_path: Option<String>,
}

/// 已配对直连终端在 `hello` 中声明的本地硬件档案与可执行能力列表。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceHardwareDeclaration {
    #[serde(default)]
    pub(super) mac: String,
    #[serde(default)]
    pub(super) chip: String,
    #[serde(default)]
    pub(super) model: String,
    #[serde(default)]
    pub(super) zone: String,
    #[serde(default)]
    pub(super) group_id: String,
    #[serde(default)]
    pub(super) vp_version: u64,
    #[serde(default)]
    pub(super) capabilities: Vec<String>,
}

fn default_command_timeout_ms() -> u32 {
    5000
}

/// Host 经已认证 WSS 向直连终端下发的本地外设控制指令。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceCommandRequest {
    pub(super) command_id: String,
    pub(super) capability: String,
    pub(super) action: String,
    #[serde(default)]
    pub(super) params_json: String,
    #[serde(default = "default_command_timeout_ms")]
    pub(super) timeout_ms: u32,
}

/// 直连终端执行本地外设控制指令后的结果回执。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceCommandResult {
    pub(super) command_id: String,
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) result_json: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) media_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) error_code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) error_message: String,
}

/// Host 下发至直连终端加密 NVS 的单个说话人声纹模板（192 维 L2 归一化 FBank 向量）。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceprintSyncTemplate {
    pub(super) speaker_id: String,
    pub(super) display_name: String,
    pub(super) role: i32,
    pub(super) embedding: Vec<f32>,
    pub(super) version: u64,
}

/// Host 经 WSS 向直连终端推送声纹模板增量/全量同步包。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceprintSyncRequest {
    pub(super) group_id: String,
    pub(super) vp_version: u64,
    pub(super) full_sync: bool,
    #[serde(default)]
    pub(super) upserts: Vec<VoiceprintSyncTemplate>,
    #[serde(default)]
    pub(super) deleted_speaker_ids: Vec<String>,
}

/// 直连终端写入加密 NVS 声纹库后的同步回执。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VoiceprintSyncAck {
    pub(super) vp_version: u64,
    pub(super) count: u32,
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// 设备对认证挑战的签名响应及本连接能力声明。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceHello {
    pub(super) device_id: String,
    pub(super) device_nonce: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    pub(super) output_preference: OutputPreferenceSnapshot,
    pub(super) client_version: String,
    pub(super) signature: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) mcp_servers: Vec<DeviceMcpServerDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) console: Option<DeviceConsoleDeclaration>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) speech_relay: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) hardware: Option<DeviceHardwareDeclaration>,
}

/// Host 完成认证和有效能力裁剪后的连接确认。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HelloAck {
    pub(super) device_id: String,
    pub(super) connection_id: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    pub(super) output_preference: OutputPreferenceSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) speech_services: Option<DeviceSpeechServicesSnapshot>,
}

/// 在线设备请求切换后续托管输出形态。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetOutputPreference {
    pub(super) output_preference: OutputPreferenceSnapshot,
}

/// Host 接受并应用当前连接输出偏好后的确认。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutputPreferenceChanged {
    pub(super) output_preference: OutputPreferenceSnapshot,
}

/// 带稳定客户端输入 ID 的设备文字输入。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TextInput {
    pub(super) client_input_id: String,
    pub(super) text: String,
    pub(super) output_preference: OutputPreferenceSnapshot,
}

/// Device wire v1 音频格式描述；当前只接受固定 PCM16/16k/mono/20ms。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PcmFormat {
    pub(super) encoding: String,
    pub(super) sample_rate_hz: u32,
    pub(super) channels: u8,
    pub(super) frame_duration_ms: u16,
}

impl PcmFormat {
    pub(super) fn is_protocol_v1(&self) -> bool {
        self.encoding == "pcm_s16le"
            && self.sample_rate_hz == 16_000
            && self.channels == 1
            && self.frame_duration_ms == 20
    }

    pub(super) fn is_relay_pcm(&self) -> bool {
        self.encoding == "pcm_s16le"
            && self.sample_rate_hz == 16_000
            && self.channels == 1
            && (self.frame_duration_ms == 0 || self.frame_duration_ms == 20)
    }
}

/// Host 通知已开启 `speech_relay` 的终端当前 ASR/TTS 可用状态变化。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechStatusChanged {
    pub(super) speech_services: DeviceSpeechServicesSnapshot,
}

/// 终端开始一次独立于 Agent 对话的语音识别中继请求。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechAsrStart {
    pub(super) request_id: String,
    pub(super) stream_id: u32,
    pub(super) format: PcmFormat,
}

/// 终端结束一次语音识别中继音频上传并等待识别结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechAsrStop {
    pub(super) request_id: String,
    pub(super) stream_id: u32,
    pub(super) last_sequence: u32,
}

/// 终端取消一次尚未完成的语音识别中继请求。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechAsrCancel {
    pub(super) request_id: String,
}

/// Host 返回给终端的语音识别中继结果或失败错误码。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechAsrResult {
    pub(super) request_id: String,
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// 终端请求 Host 使用本机 TTS 服务合成短文本 PCM16/16k/mono。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechTtsRequest {
    pub(super) request_id: String,
    pub(super) stream_id: u32,
    pub(super) text: String,
}

/// Host 通知终端准备接收语音合成中继下行 PCM 分片或返回前置拒绝错误。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechTtsStart {
    pub(super) request_id: String,
    pub(super) stream_id: u32,
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) format: Option<PcmFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// Host 通知终端一次语音合成中继流传输完成或中止。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpeechTtsEnd {
    pub(super) request_id: String,
    pub(super) stream_id: u32,
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// 开始一段按键说话音频流，并冻结该轮输出偏好。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListenStart {
    pub(super) client_input_id: String,
    pub(super) stream_id: u32,
    pub(super) format: PcmFormat,
    pub(super) output_preference: OutputPreferenceSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) speaker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) speaker_role: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) speaker_score: Option<f32>,
}

/// 设备已发送一段音频的最后序号，请求 Host 完成接管和识别。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListenStop {
    pub(super) stream_id: u32,
    pub(super) last_sequence: u32,
}

/// 设备放弃当前上行音频段；Host 必须丢弃未提交 PCM。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListenCancel {
    pub(super) stream_id: u32,
}

/// 设备主动取消当前及排队中的播放。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackCancel {
    pub(super) output_id: String,
    pub(super) stream_id: u32,
}

/// Host 宣告一段下行 PCM 的身份、格式与文本辅助信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackStart {
    pub(super) output_id: String,
    pub(super) run_id: RunId,
    pub(super) stream_id: u32,
    pub(super) format: PcmFormat,
    pub(super) text: String,
    pub(super) sample_count: u64,
}

/// Host 宣告一段播放结束及其结束原因。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlaybackEnd {
    pub(super) output_id: String,
    pub(super) stream_id: u32,
    pub(super) reason: String,
}

/// Host 返回给设备用于界面展示的 ASR 转写，不等同于 Runtime Input 确认。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Transcript {
    pub(super) client_input_id: String,
    pub(super) text: String,
}

/// 从设备上行二进制消息借用出的 PCM 帧视图。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UplinkPcmFrame<'a> {
    pub(super) stream_id: u32,
    pub(super) sequence: u32,
    pub(super) payload: &'a [u8],
}

impl<'a> UplinkPcmFrame<'a> {
    pub(super) fn decode(frame: &'a [u8]) -> Result<Self, ProtocolError> {
        if frame.len() != PCM_FRAME_BYTES {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        if frame[0] != 1
            || frame[1] != 1
            || u16::from_be_bytes([frame[2], frame[3]]) != 0
            || usize::from(u16::from_be_bytes([frame[12], frame[13]])) != PCM_PAYLOAD_BYTES
            || u16::from_be_bytes([frame[14], frame[15]]) != 0
        {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        Ok(Self {
            stream_id: u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]),
            sequence: u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]),
            payload: &frame[PCM_HEADER_BYTES..],
        })
    }
}

/// Host 下行 PCM 帧编码器；无实例状态。
pub(super) struct DownlinkPcmFrame;

impl DownlinkPcmFrame {
    pub(super) fn encode(
        stream_id: u32,
        sequence: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, ProtocolError> {
        if stream_id == 0
            || payload.is_empty()
            || payload.len() > PCM_PAYLOAD_BYTES
            || !payload.len().is_multiple_of(2)
        {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        let mut frame = vec![0_u8; PCM_FRAME_BYTES];
        frame[0] = 1;
        frame[1] = 2;
        frame[4..8].copy_from_slice(&stream_id.to_be_bytes());
        frame[8..12].copy_from_slice(&sequence.to_be_bytes());
        frame[12..14].copy_from_slice(&(PCM_PAYLOAD_BYTES as u16).to_be_bytes());
        frame[PCM_HEADER_BYTES..PCM_HEADER_BYTES + payload.len()].copy_from_slice(payload);
        Ok(frame)
    }
}

/// 从设备上行语音中继二进制消息借用出的变长 PCM 帧视图（kind = 3）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RelayUplinkPcmFrame<'a> {
    pub(super) stream_id: u32,
    pub(super) sequence: u32,
    pub(super) payload: &'a [u8],
}

impl<'a> RelayUplinkPcmFrame<'a> {
    pub(super) fn decode(frame: &'a [u8]) -> Result<Self, ProtocolError> {
        if frame.len() <= PCM_HEADER_BYTES {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        if frame[0] != 1
            || frame[1] != PCM_FRAME_KIND_RELAY_UPLINK
            || u16::from_be_bytes([frame[2], frame[3]]) != 0
            || u16::from_be_bytes([frame[14], frame[15]]) != 0
        {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        let payload_len = usize::from(u16::from_be_bytes([frame[12], frame[13]]));
        if payload_len == 0
            || payload_len > RELAY_UPLINK_MAX_PAYLOAD_BYTES
            || !payload_len.is_multiple_of(2)
            || frame.len() != PCM_HEADER_BYTES + payload_len
        {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        let stream_id = u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]);
        if stream_id == 0 {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        Ok(Self {
            stream_id,
            sequence: u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]),
            payload: &frame[PCM_HEADER_BYTES..],
        })
    }
}

/// Host 下行语音中继变长 PCM 帧编码器（kind = 4）。
pub(super) struct RelayDownlinkPcmFrame;

impl RelayDownlinkPcmFrame {
    pub(super) fn encode(
        stream_id: u32,
        sequence: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, ProtocolError> {
        if stream_id == 0
            || payload.is_empty()
            || payload.len() > RELAY_UPLINK_MAX_PAYLOAD_BYTES
            || !payload.len().is_multiple_of(2)
        {
            return Err(ProtocolError::InvalidPcmFrame);
        }
        let mut frame = vec![0_u8; PCM_HEADER_BYTES + payload.len()];
        frame[0] = 1;
        frame[1] = PCM_FRAME_KIND_RELAY_DOWNLINK;
        frame[4..8].copy_from_slice(&stream_id.to_be_bytes());
        frame[8..12].copy_from_slice(&sequence.to_be_bytes());
        frame[12..14].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        frame[PCM_HEADER_BYTES..].copy_from_slice(payload);
        Ok(frame)
    }
}

/// Runtime 已可靠暂存设备输入的确认；实际领取前没有 Run。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InputAccepted {
    pub(super) client_input_id: String,
    pub(super) input_id: InputId,
    pub(super) run_id: Option<RunId>,
    /// queued 表示可撤销暂存；已经领取的幂等结果可返回已有 Run 状态。
    pub(super) queue_state: String,
}

impl InputAccepted {
    /// 只投影已提交的输入回执；暂存阶段没有执行身份，不能为设备补造 Run。
    pub(super) fn from_submission(client_input_id: String, result: SubmitInputResult) -> Self {
        let queue_state = match result.run.as_ref().map(|run| run.status) {
            None => "queued",
            Some(RunStatus::Accepted) => "accepted",
            Some(RunStatus::Running) => "running",
            Some(RunStatus::Cancelling) => "cancelling",
            Some(RunStatus::Completed) => "completed",
            Some(RunStatus::Failed) => "failed",
            Some(RunStatus::Cancelled) => "cancelled",
            Some(RunStatus::Interrupted) => "interrupted",
            Some(RunStatus::CompactionRequired) => "compaction_required",
        };
        Self {
            client_input_id,
            input_id: result.input_id,
            run_id: result.run.map(|run| run.run_id),
            queue_state: queue_state.to_owned(),
        }
    }
}

/// Host 已完整接管一段上行 PCM；终端可释放该段本地缓存。
///
/// 这不是 Runtime Input 的确认，也不表示已经发起 LLM Run。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InputSegmentAccepted {
    pub(super) client_input_id: String,
    pub(super) stream_id: u32,
}

/// Runtime 规范 Assistant 正文向设备文字渠道的投递。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TextOutput {
    pub(super) output_id: String,
    pub(super) run_id: RunId,
    pub(super) text: String,
}

/// Host 向设备投影的交互阶段，不是 Runtime Run 权威状态机。
///
/// `reason` 只使用协议稳定短码，不能携带 Provider 或内部错误详情。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InteractionStateChanged {
    pub(super) run_id: Option<RunId>,
    pub(super) client_input_id: Option<String>,
    pub(super) state: String,
    pub(super) reason: Option<String>,
}

/// 应用层保活探针；区别于 WebSocket 自身的 Ping/Pong 帧。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplicationPing {
    pub(super) nonce: String,
    pub(super) sent_at_ms: i64,
}

/// Device wire 的脱敏错误响应。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireError {
    pub(super) code: String,
    pub(super) correlation_message_id: Option<String>,
    pub(super) recoverable: bool,
}

pub(super) fn preference_is_supported(
    capabilities: DeviceCapabilitiesSnapshot,
    preference: OutputPreferenceSnapshot,
) -> bool {
    match preference {
        OutputPreferenceSnapshot::Text => capabilities.output_text,
        OutputPreferenceSnapshot::Audio => capabilities.output_pcm16_16k_mono,
        OutputPreferenceSnapshot::TextAndAudio => {
            capabilities.output_text && capabilities.output_pcm16_16k_mono
        }
    }
}

/// 控制包和 PCM 帧在进入连接状态机前的协议校验错误。
#[derive(Debug, Error, Eq, PartialEq)]
pub(super) enum ProtocolError {
    #[error("control message is too large")]
    MessageTooLarge,
    #[error("control message is not valid JSON")]
    InvalidJson,
    #[error("control envelope is invalid")]
    InvalidEnvelope,
    #[error("control payload is invalid")]
    InvalidPayload,
    #[error("protocol major is unsupported")]
    UnsupportedProtocol,
    #[error("message id was already used on this connection")]
    DuplicateMessageId,
    #[error("PCM frame is invalid")]
    InvalidPcmFrame,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_acknowledgement_does_not_invent_a_run() {
        let acknowledgement = InputAccepted::from_submission(
            "device-input".to_owned(),
            SubmitInputResult {
                user_message: None,
                generation: None,
                not_executed: None,
                input_id: InputId::new("queued-input").unwrap(),
                run: None,
            },
        );
        let json = serde_json::to_value(acknowledgement).unwrap();
        assert_eq!(json["queue_state"], "queued");
        assert_eq!(json["input_id"], "queued-input");
        assert!(json["run_id"].is_null());
    }

    #[derive(Deserialize)]
    struct EnvelopeFixture {
        valid_envelope: Envelope,
        error_codes: Vec<ErrorCodeFixture>,
    }

    #[derive(Deserialize)]
    struct ErrorCodeFixture {
        code: String,
        recoverable: bool,
    }

    #[derive(Deserialize)]
    struct PcmFixture {
        header_hex: String,
        downlink_header_hex: String,
        stream_id: u32,
        sequence: u32,
        payload_bytes: usize,
        invalid_header_hex: Vec<String>,
    }

    #[test]
    fn envelope_is_strict_bounded_and_replay_checked() {
        let envelope = Envelope::new(
            "message-1".to_owned(),
            "ping",
            &ApplicationPing {
                nonce: "nonce".to_owned(),
                sent_at_ms: 1,
            },
        )
        .expect("envelope");
        let encoded = envelope.encode().expect("encode");
        let decoded = Envelope::decode(&encoded).expect("decode");
        assert_eq!(decoded.message_type, "ping");
        assert!(decoded.payload::<ApplicationPing>().is_ok());
        assert!(matches!(
            Envelope::decode(
                r#"{"protocol_major":2,"protocol_minor":0,"message_id":"m","type":"ping","payload":{}}"#
            ),
            Err(ProtocolError::UnsupportedProtocol)
        ));
        assert!(matches!(
            Envelope::decode(
                r#"{"protocol_major":1,"protocol_minor":0,"message_id":"m","type":"ping","payload":{},"extra":true}"#
            ),
            Err(ProtocolError::InvalidJson)
        ));
        let mut replay = MessageReplayWindow::default();
        replay.accept("message-1").expect("first");
        assert_eq!(
            replay.accept("message-1"),
            Err(ProtocolError::DuplicateMessageId)
        );
    }

    #[test]
    fn text_input_payload_is_strict_and_preserves_the_client_identity() {
        let envelope = Envelope::new(
            "message-text-input".to_owned(),
            "text_input",
            &TextInput {
                client_input_id: "client-input-stable".to_owned(),
                text: "hello controller".to_owned(),
                output_preference: OutputPreferenceSnapshot::Text,
            },
        )
        .expect("text input envelope");
        let decoded = Envelope::decode(&envelope.encode().expect("encode")).expect("decode");
        let input = decoded.payload::<TextInput>().expect("text input payload");
        assert_eq!(input.client_input_id, "client-input-stable");
        assert_eq!(input.text, "hello controller");
        assert_eq!(input.output_preference, OutputPreferenceSnapshot::Text);
        assert!(
            serde_json::from_value::<TextInput>(serde_json::json!({
                "client_input_id": "client-input-stable",
                "text": "hello controller",
                "output_preference": "text",
                "session_id": "forbidden-device-selected-session"
            }))
            .is_err()
        );
    }

    #[test]
    fn shared_node_rust_envelope_and_error_fixture_matches() {
        let fixture: EnvelopeFixture = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/resources/device-protocol-v1/fixtures/envelope-v1.json"
        )))
        .expect("fixture");
        let encoded = fixture.valid_envelope.encode().expect("encode");
        let decoded = Envelope::decode(&encoded).expect("decode");
        let error = decoded.payload::<WireError>().expect("wire error");
        assert_eq!(decoded.message_type, "error");
        assert_eq!(error.code, "authentication_failed");
        assert_eq!(
            error.correlation_message_id.as_deref(),
            Some("message-hello-fixture")
        );
        assert!(!error.recoverable);
        assert!(
            fixture.error_codes.iter().any(|entry| {
                entry.code == error.code && entry.recoverable == error.recoverable
            })
        );
        assert!(
            fixture
                .error_codes
                .iter()
                .any(|entry| entry.code == "pairing_failed" && entry.recoverable)
        );
        assert!(
            fixture
                .error_codes
                .iter()
                .any(|entry| entry.code == "device_revoked" && !entry.recoverable)
        );
    }

    #[test]
    fn pcm_header_is_network_order_and_payload_is_little_endian_opaque() {
        let fixture: PcmFixture = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/resources/device-protocol-v1/fixtures/pcm-v1.json"
        )))
        .expect("PCM fixture");
        let header = decode_hex(&fixture.header_hex);
        let mut bytes = vec![0_u8; PCM_FRAME_BYTES];
        bytes[..PCM_HEADER_BYTES].copy_from_slice(&header);
        bytes[PCM_HEADER_BYTES..PCM_HEADER_BYTES + 4].copy_from_slice(&[1, 2, 3, 4]);
        let decoded = UplinkPcmFrame::decode(&bytes).expect("valid PCM frame");
        assert_eq!(decoded.stream_id, fixture.stream_id);
        assert_eq!(decoded.sequence, fixture.sequence);
        assert_eq!(decoded.payload.len(), fixture.payload_bytes);
        assert_eq!(&decoded.payload[..4], &[1, 2, 3, 4]);
        let encoded = DownlinkPcmFrame::encode(fixture.stream_id, fixture.sequence, &[1, 2, 3, 4])
            .expect("downlink PCM frame");
        assert_eq!(
            &encoded[..PCM_HEADER_BYTES],
            decode_hex(&fixture.downlink_header_hex)
        );
        assert_eq!(
            &encoded[PCM_HEADER_BYTES..PCM_HEADER_BYTES + 4],
            &[1, 2, 3, 4]
        );
        assert!(
            encoded[PCM_HEADER_BYTES + 4..]
                .iter()
                .all(|byte| *byte == 0)
        );
        for invalid in fixture.invalid_header_hex {
            bytes[..PCM_HEADER_BYTES].copy_from_slice(&decode_hex(&invalid));
            assert_eq!(
                UplinkPcmFrame::decode(&bytes),
                Err(ProtocolError::InvalidPcmFrame)
            );
        }
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
        assert!(remainder.is_empty(), "hex must contain complete bytes");
        pairs
            .iter()
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hex"), 16)
                    .expect("valid hex")
            })
            .collect()
    }
}
