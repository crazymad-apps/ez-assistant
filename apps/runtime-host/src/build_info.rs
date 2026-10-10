//! 发布脚本读取的无副作用构建信息；不初始化 Runtime Home 或应用协议状态。

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct BuildInfo {
    #[serde(flatten)]
    compatibility: assistant_protocol::ClientCompatibility,
    os: &'static str,
    arch: &'static str,
    enterprise_protocols: &'static [u32],
}

impl BuildInfo {
    pub(crate) fn current() -> Self {
        Self {
            compatibility: assistant_protocol::ClientCompatibility::current(),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            enterprise_protocols: crate::access::enterprise::SUPPORTED_PROTOCOLS,
        }
    }
}
