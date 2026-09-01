//! Runtime 持有的稳定设备登记事实。

use assistant_protocol::{DeviceId, SessionId};

/// Runtime 持久设备登记的业务生命周期，与 Host 当前是否在线无关。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceLifecycle {
    /// 已完成密钥绑定，可用于认证和业务输入。
    Paired,
    /// 登记记录保留用于审计和拒绝旧凭据，但不再允许认证。
    Revoked,
}

/// Ed25519 公钥的原始 32-byte 编码。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePublicKey([u8; 32]);

impl DevicePublicKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }
}

/// Runtime 中一台设备的稳定登记事实。
///
/// 在线连接、心跳、能力协商和播放状态由 Host 持有，不写入该结构。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairedDevice {
    pub device_id: DeviceId,
    pub display_name: String,
    pub public_key: DevicePublicKey,
    pub lifecycle: DeviceLifecycle,
    pub paired_at_ms: i64,
    pub updated_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

/// 完成配对协议后提交给 Store 的新设备登记。
///
/// 只有设备证明长期私钥持有权并完成最终 commit 后才能构造该写入意图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPairedDevice {
    pub device_id: DeviceId,
    pub display_name: String,
    pub public_key: DevicePublicKey,
    pub paired_at_ms: i64,
}

/// 重命名一台仍处于配对状态设备的持久化意图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceNameChange {
    pub device_id: DeviceId,
    pub display_name: String,
    pub changed_at_ms: i64,
}

/// 吊销设备身份的持久化意图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRevocation {
    pub device_id: DeviceId,
    pub revoked_at_ms: i64,
}

/// 设备吊销事务的完整结果。
///
/// 同一事务会清除所有指向该设备的 PC 输出托管，调用方据此刷新对应 Session 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRevocationResult {
    /// 吊销后的稳定设备记录；重复吊销会返回同一终态。
    pub device: PairedDevice,
    /// 同一事务中被解除 PC 输出托管的 Controller Session。
    pub cleared_session_ids: Vec<SessionId>,
    /// 本次调用是否真正造成了持久状态变化。
    pub changed: bool,
}

/// Controller 当前稳定的 PC 输出附加托管目标。
///
/// 它不改变输入来源优先路由，也不取代 Desktop 对全部显式输入输出的保留。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PcOutputHosting {
    pub device_id: DeviceId,
    pub device_name: String,
}

/// 设置或解除一个 Controller Session 的 PC 输出托管意图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PcOutputHostingChange {
    pub controller_session_id: SessionId,
    pub device_id: Option<DeviceId>,
}
