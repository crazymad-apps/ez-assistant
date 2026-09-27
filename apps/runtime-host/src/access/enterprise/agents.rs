//! 企业智能体来源只引用当前 Credentials；迟到的旧登录结果不可发布。

use std::sync::Arc;

use assistant_protocol::PluginPackageSelection;
use assistant_runtime::{
    CenterCustomAgentDirectory, CustomAgentDirectorySource, RuntimeError, RuntimeResult,
};
use futures_util::future::BoxFuture;

use super::{Credentials, UserKey};

pub(crate) struct HostAgentDirectorySource {
    credentials: Arc<Credentials>,
    key: UserKey,
}

impl HostAgentDirectorySource {
    pub(crate) fn new(credentials: Arc<Credentials>, key: UserKey) -> Self {
        Self { credentials, key }
    }
}

impl CustomAgentDirectorySource for HostAgentDirectorySource {
    fn can_use_cached(&self, center_id: &str) -> bool {
        self.credentials
            .current_agent_center(&self.key)
            .is_ok_and(|(_, _, current_center_id)| current_center_id == center_id)
    }

    fn load(&self) -> BoxFuture<'_, RuntimeResult<CenterCustomAgentDirectory>> {
        Box::pin(async move {
            let (center, token, center_id) = self
                .credentials
                .current_agent_center(&self.key)
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            let agents = center
                .list_runtime_agents(&token)
                .await
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            let (_, current_token, current_center_id) = self
                .credentials
                .current_agent_center(&self.key)
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            if current_center_id != center_id || current_token.expose() != token.expose() {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
            Ok(CenterCustomAgentDirectory { center_id, agents })
        })
    }

    fn resolve_plugin(
        &self,
        center_id: &str,
        name: &str,
        required_skills: Vec<String>,
        required_mcp: Vec<String>,
    ) -> BoxFuture<'_, RuntimeResult<PluginPackageSelection>> {
        let center_id = center_id.to_owned();
        let name = name.to_owned();
        Box::pin(async move {
            let (center, token, current_center_id) = self
                .credentials
                .current_agent_center(&self.key)
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            if current_center_id != center_id {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
            let selected = center
                .resolve_plugin_with_requirements(
                    &token,
                    &center_id,
                    &name,
                    None,
                    &required_skills,
                    &required_mcp,
                )
                .await
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            let (_, current_token, current_center_id) = self
                .credentials
                .current_agent_center(&self.key)
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?;
            if current_center_id != center_id || current_token.expose() != token.expose() {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
            Ok(selected)
        })
    }
}
