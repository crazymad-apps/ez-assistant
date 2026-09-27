//! 企业智能体完整目录的可信来源端口；中心网络和登录令牌留在 Host。

use futures_util::future::BoxFuture;

use assistant_protocol::CustomAgentConfig;
use assistant_protocol::PluginPackageSelection;

use crate::RuntimeResult;

#[derive(Clone)]
pub struct CenterCustomAgentDirectory {
    pub center_id: String,
    pub agents: Vec<CustomAgentConfig>,
}

pub trait CustomAgentDirectorySource: Send + Sync {
    fn load(&self) -> BoxFuture<'_, RuntimeResult<CenterCustomAgentDirectory>>;

    fn can_use_cached(&self, _center_id: &str) -> bool {
        true
    }

    fn resolve_plugin(
        &self,
        _center_id: &str,
        _name: &str,
        _required_skills: Vec<String>,
        _required_mcp: Vec<String>,
    ) -> BoxFuture<'_, RuntimeResult<PluginPackageSelection>> {
        Box::pin(async { Err(crate::RuntimeError::ConfigurationUnavailable) })
    }
}
