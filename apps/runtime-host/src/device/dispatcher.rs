//! Runtime 附加 Channel 输出到在线 Device 连接的非持久分发适配。

use std::sync::{Arc, OnceLock, Weak};

use assistant_runtime::{
    ChannelOutput, ChannelOutputDispatchError, ChannelOutputDispatcher, ChannelOutputFuture,
    ChannelSpeechRequirementFuture, ChannelSpeechSegment, ResolvedChannelDelivery,
};

use super::gateway::GatewayShared;

/// Runtime 在 Host 入口开放前注入本对象；Gateway 装配完成后只绑定一个弱连接资源引用。
/// 弱引用避免形成 `Runtime -> Dispatcher -> Gateway -> Runtime` 的所有权环。
pub(crate) struct DeviceChannelOutputDispatcher {
    gateway: OnceLock<Weak<GatewayShared>>,
}

impl DeviceChannelOutputDispatcher {
    pub(crate) fn new() -> Self {
        Self {
            gateway: OnceLock::new(),
        }
    }

    pub(super) fn bind(&self, gateway: &Arc<GatewayShared>) {
        self.gateway
            .set(Arc::downgrade(gateway))
            .expect("device channel dispatcher is bound exactly once");
    }
}

impl ChannelOutputDispatcher for DeviceChannelOutputDispatcher {
    fn dispatch(&self, output: ChannelOutput) -> ChannelOutputFuture {
        let gateway = self.gateway.get().and_then(Weak::upgrade);
        Box::pin(async move {
            let gateway = gateway.ok_or(ChannelOutputDispatchError::Unavailable)?;
            gateway.dispatch_channel_output(output).await
        })
    }

    fn dispatch_speech(&self, segment: ChannelSpeechSegment) -> ChannelOutputFuture {
        let gateway = self.gateway.get().and_then(Weak::upgrade);
        Box::pin(async move {
            let gateway = gateway.ok_or(ChannelOutputDispatchError::Unavailable)?;
            gateway.dispatch_speech_segment(segment).await
        })
    }

    fn requires_speech(
        &self,
        deliveries: Vec<ResolvedChannelDelivery>,
    ) -> ChannelSpeechRequirementFuture {
        let gateway = self.gateway.get().and_then(Weak::upgrade);
        Box::pin(async move {
            match gateway {
                Some(gateway) => gateway.requires_speech(&deliveries).await,
                None => false,
            }
        })
    }
}
