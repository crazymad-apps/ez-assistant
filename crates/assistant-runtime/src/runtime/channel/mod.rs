//! Runtime 内部的渠道目标解析与 `speak` 工具装配边界。
//!
//! 本模块只维护与规范会话相关的输出语义：根据当前输出周期解析实际渠道目标，并向 Runtime
//! 父模块暴露私有 `speak` 工具。TTS、设备连接、FIFO 播放队列和取消播放均由 Host 持有；
//! 播报片段不会在 Runtime 中形成第二份 Assistant 正文或可恢复的媒体队列。

mod speak;

pub(super) use speak::SpeakTool;
pub(crate) use speak::{
    MAX_SPEAK_SEGMENTS_PER_OUTPUT_CYCLE, MAX_SPEAK_TEXT_CHARS, SpeakAuthorizationFacts,
};

use crate::{
    DeviceDeliveryPreference, InputChannelSource, ReplyRoute, ResolvedChannelDelivery,
    RuntimeError, RuntimeResult, session::SessionState,
};

/// 根据输出周期的来源事实解析当前一次渠道投递目标。
///
/// 设备输入优先回到来源设备，并使用该 Input 冻结的输出偏好；Desktop 输入以及
/// `SessionDefault` 代理报告读取调用此函数时的 PC 托管关系；显式 Device 路由则保持其冻结目标。
/// 返回空集合只表示无需附加渠道投递，Desktop 中的规范 Conversation 正文仍由既有链路保留。
pub(crate) fn resolve_output_cycle_deliveries(
    state: &SessionState,
) -> RuntimeResult<Vec<ResolvedChannelDelivery>> {
    let cycle = state
        .output_cycle
        .as_ref()
        .ok_or(RuntimeError::InvalidRequest {
            reason: "session has no active output cycle",
        })?;
    let source =
        state
            .inputs
            .get(&cycle.source_input_id)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "output cycle source input",
            })?;
    Ok(match source.stored.channel_source.as_ref() {
        Some(InputChannelSource::Device(source)) => vec![ResolvedChannelDelivery::Device {
            device_id: source.device_id.clone(),
            preference: DeviceDeliveryPreference::Frozen(source.requested_output),
        }],
        Some(InputChannelSource::Desktop(_)) => resolve_session_default_delivery(state),
        None => match source.stored.cross_session.as_ref() {
            Some(envelope)
                if matches!(
                    envelope.binding,
                    crate::CrossSessionInputBinding::ProxyReport { .. }
                ) =>
            {
                match &envelope.reply_route {
                    ReplyRoute::SessionDefault => resolve_session_default_delivery(state),
                    ReplyRoute::Device {
                        device_id,
                        requested_output,
                    } => vec![ResolvedChannelDelivery::Device {
                        device_id: device_id.clone(),
                        preference: DeviceDeliveryPreference::Frozen(*requested_output),
                    }],
                }
            }
            _ => Vec::new(),
        },
    })
}

/// 按当前 Session 此刻的默认托管关系生成投递，因此同一输出周期允许用户中途切换渠道。
fn resolve_session_default_delivery(state: &SessionState) -> Vec<ResolvedChannelDelivery> {
    state
        .pc_output_hosting
        .as_ref()
        .map(|hosting| ResolvedChannelDelivery::Device {
            device_id: hosting.device_id.clone(),
            preference: DeviceDeliveryPreference::Preset,
        })
        .into_iter()
        .collect()
}
