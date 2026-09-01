//! 传输无关的 Channel 输入来源与附加输出端口。

use std::{future::Future, pin::Pin};

use agent_types::ToolCallId;
use assistant_protocol::{DeviceId, InputId, RunId, SessionId, SubmitInputRequest};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Runtime 已接收正文所对应的原始输入形态。
///
/// 语音在进入 Runtime 前已经由 Host 完成 ASR，因此这里只记录来源语义，不承载音频数据。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Text,
    SpeechTranscript,
}

/// 输入来源为本轮回复声明的期望输出形态。
///
/// 该值描述用户/渠道意图；Host 仍会根据目标在线状态和真实能力决定能否投递或降级。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputPreference {
    Text,
    Audio,
    TextAndAudio,
}

/// 设备输入在进入 Session 时冻结的来源事实。
///
/// `client_input_id` 用于设备重试去重，输出偏好随本次输入冻结，不随设备后续切换而改变。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceInputSource {
    pub device_id: DeviceId,
    pub client_input_id: String,
    pub modality: InputModality,
    pub requested_output: OutputPreference,
}

/// Desktop 输入在进入 Session 时冻结的来源事实。
///
/// Desktop 的请求幂等键由既有 `SubmitInputRequest` 单独承载，因此这里不重复保存客户端输入 ID。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesktopInputSource {
    pub modality: InputModality,
    pub requested_output: OutputPreference,
}

/// 一条 Session 输入所属的交互渠道。
///
/// Desktop 和 Device 都是会话的输入渠道，不由设备自行选择目标 Session。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputChannelSource {
    Desktop(DesktopInputSource),
    Device(DeviceInputSource),
}

/// 向既有 Session 提交一条已经由 Host 识别来源的输入。
///
/// `input` 保留既有产品输入意图，`source` 只补充可信交互渠道事实。Desktop、Device 和未来
/// 其他渠道都通过这一入口进入同一套 Conversation/Input/Run 持久化路径。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitSessionInputRequest {
    pub input: SubmitInputRequest,
    pub source: InputChannelSource,
}

impl InputChannelSource {
    /// 构造当前 Desktop HTTP 文本入口的来源事实。
    #[must_use]
    pub fn desktop_text() -> Self {
        Self::Desktop(DesktopInputSource {
            modality: InputModality::Text,
            requested_output: OutputPreference::Text,
        })
    }
}

/// 随跨 Session 入队请求传递的回复路径。
///
/// `ControllerDelivery` 冻结该路径，Goal 自动推进在同一 Input/Run 中读取它，`ProxyReport`
/// 原样传回；目标 Session 每次执行 `speak` 或最终文字投递时再解析实际渠道目标。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyRoute {
    /// 没有显式来源渠道；每次投递时读取目标 Session 当时的默认托管对象。
    #[default]
    SessionDefault,
    /// 回复输入来源设备，并沿用输入接受时冻结的偏好。
    Device {
        device_id: DeviceId,
        requested_output: OutputPreference,
    },
}

/// 设备投递时采用的输出偏好解析时机。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceDeliveryPreference {
    /// 使用输入接受时冻结的偏好，适用于“回复输入来源设备”。
    Frozen(OutputPreference),
    /// 在 Host 实际投递时读取设备当前预设，适用于 PC 输出托管。
    Preset,
}

/// Runtime 在一次实际投递前解析完成的附加渠道目标。
///
/// Desktop 的规范消息始终由既有 Conversation 投影展示，因此这里只描述额外渠道投递。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedChannelDelivery {
    /// 向一台设备附加投递；是否在线和能力降级由 Host 最终判断。
    Device {
        device_id: DeviceId,
        preference: DeviceDeliveryPreference,
    },
}

/// Runtime 在一个逻辑输出周期结束后交给 Host 的最终投递指令。
///
/// `assistant_text` 仍是 Conversation 中的规范正文；本结构只承载投递关联，不保存第二份正文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelOutput {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub assistant_text: Option<String>,
    /// 当前输出周期是否已经至少成功提交一段播报。
    pub speech_completed: bool,
    pub deliveries: Vec<ResolvedChannelDelivery>,
}

/// 一次 `speak` Tool Call 形成的易失播报片段。
///
/// 片段按可靠 Tool 执行顺序交给 Host；它不是第二份 Assistant 正文，也不进入公共协议。
#[derive(Clone, Debug)]
pub struct ChannelSpeechSegment {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub segment_id: ToolCallId,
    pub text: String,
    pub deliveries: Vec<ResolvedChannelDelivery>,
    pub cancellation: CancellationToken,
}

/// 渠道投递异步操作的擦除类型。
pub type ChannelOutputFuture =
    Pin<Box<dyn Future<Output = Result<(), ChannelOutputDispatchError>> + Send>>;

/// Host 拒绝或中止渠道投递时返回给 Runtime 的有限错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelOutputDispatchError {
    /// 目标渠道或其所需能力当前不可用。
    Unavailable,
    /// 所属 Run、输出周期或设备播放已被取消。
    Cancelled,
}

/// Runtime 向 Host 请求渠道投递的传输无关端口。
///
/// 实现方拥有连接、播放队列和语音服务状态；Runtime 不复制这些易失状态。
pub trait ChannelOutputDispatcher: Send + Sync {
    fn dispatch(&self, output: ChannelOutput) -> ChannelOutputFuture;

    fn dispatch_speech(&self, segment: ChannelSpeechSegment) -> ChannelOutputFuture;

    /// 按 Host 当前在线连接与输出偏好判断这些 delivery 是否真实需要音频。
    /// Runtime 只消费布尔结果，不复制连接、能力或偏好状态。
    fn requires_speech(
        &self,
        deliveries: Vec<ResolvedChannelDelivery>,
    ) -> ChannelSpeechRequirementFuture;
}

/// 查询一组目标是否真实需要语音的异步结果。
pub type ChannelSpeechRequirementFuture = Pin<Box<dyn Future<Output = bool> + Send>>;

/// 未装配外部渠道时使用的空实现，保持纯 Desktop Runtime 的既有行为。
pub(crate) struct NoopChannelOutputDispatcher;

impl ChannelOutputDispatcher for NoopChannelOutputDispatcher {
    fn dispatch(&self, _output: ChannelOutput) -> ChannelOutputFuture {
        Box::pin(async { Ok(()) })
    }

    fn dispatch_speech(&self, _segment: ChannelSpeechSegment) -> ChannelOutputFuture {
        Box::pin(async { Ok(()) })
    }

    fn requires_speech(
        &self,
        _deliveries: Vec<ResolvedChannelDelivery>,
    ) -> ChannelSpeechRequirementFuture {
        Box::pin(async { false })
    }
}

/// 同一 Run 的多个 AgentExecution 之间保存的单个逻辑输出周期状态。
///
/// Goal 自动推进和隐藏播报提醒仍属于同一周期；最终投递完成或失败后才清除。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputCycleState {
    /// 发起本周期的外部输入；同 Run continuation 始终保持该投递关联。
    pub source_input_id: InputId,
    /// 是否已有至少一个 `speak` 片段被 Host 成功接收。
    pub has_speech: bool,
    /// 当前输出周期已经成功进入渠道队列的播报片段数。
    pub speech_segment_count: usize,
    /// Host 已明确取消本周期播报；这是正常接管，不应再触发隐藏补播提醒。
    pub speech_cancelled: bool,
    /// 是否已经为本周期生成过一次隐藏播报提醒，防止无限续跑。
    pub speech_reminder_issued: bool,
    /// 缺失播报触发轮内续跑时，保留已经完成的规范 Assistant 正文供最终渠道投递。
    pub pending_assistant_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_source_keeps_the_existing_flat_persisted_shape() {
        let source = InputChannelSource::Desktop(DesktopInputSource {
            modality: InputModality::SpeechTranscript,
            requested_output: OutputPreference::TextAndAudio,
        });

        let value = serde_json::to_value(&source).expect("serialize Desktop source");
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "desktop",
                "modality": "speech_transcript",
                "requested_output": "text_and_audio"
            })
        );
        let decoded: InputChannelSource =
            serde_json::from_value(value).expect("deserialize Desktop source");
        assert_eq!(decoded, source);
    }

    #[test]
    fn reply_routes_have_canonical_persisted_shapes() {
        let session_default = serde_json::to_value(ReplyRoute::SessionDefault)
            .expect("serialize Session default reply route");
        assert_eq!(
            session_default,
            serde_json::json!({ "kind": "session_default" })
        );
        assert_eq!(
            serde_json::from_value::<ReplyRoute>(session_default)
                .expect("deserialize Session default reply route"),
            ReplyRoute::SessionDefault
        );

        let device = ReplyRoute::Device {
            device_id: DeviceId::new("device-route").expect("valid Device ID"),
            requested_output: OutputPreference::Audio,
        };
        let serialized = serde_json::to_value(&device).expect("serialize Device reply route");
        assert_eq!(
            serialized,
            serde_json::json!({
                "kind": "device",
                "device_id": "device-route",
                "requested_output": "audio"
            })
        );
        assert_eq!(
            serde_json::from_value::<ReplyRoute>(serialized)
                .expect("deserialize Device reply route"),
            device
        );
    }
}
