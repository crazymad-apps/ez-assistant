//! 配置来源适配：业务读取统一的服务商、默认选择和固定参数，不感知数据库或远程来源。

use assistant_protocol::{ModelFixedConfig, ModelSelection, ProviderInstanceId};

use super::{ConfigRegistry, ModelSource};
use crate::{RuntimeError, RuntimeResult, RuntimeStore};

impl ConfigRegistry {
    pub(crate) fn effective_model_selection(
        &self,
        requested: Option<&ModelSelection>,
    ) -> RuntimeResult<Option<ModelSelection>> {
        let state = self.managed_models()?;
        let requested = if state
            .settings
            .management
            .as_ref()
            .is_some_and(|policy| policy.read_only)
        {
            None
        } else {
            requested
        };
        Ok(requested.or(state.settings.default_model.as_ref()).cloned())
    }

    pub(crate) async fn fixed_model(
        &self,
        selection: ModelSelection,
        store: &dyn RuntimeStore,
    ) -> RuntimeResult<Option<ModelFixedConfig>> {
        let remote = {
            let state = self.managed_models()?;
            (state.source == ModelSource::External).then(|| state.external.clone())
        };
        if let Some(configuration) = remote {
            return Ok(configuration
                .filter(|value| value.selection() == &selection)
                .map(|value| value.fixed()));
        }
        store
            .get_model_fixed_config(selection)
            .await
            .map_err(|error| RuntimeError::from_store("load model parameters", error))
    }

    pub(crate) async fn fixed_models(
        &self,
        provider: ProviderInstanceId,
        offset: u32,
        limit: u32,
        store: &dyn RuntimeStore,
    ) -> RuntimeResult<Vec<ModelFixedConfig>> {
        let remote = {
            let state = self.managed_models()?;
            (state.source == ModelSource::External).then(|| state.external.clone())
        };
        if let Some(configuration) = remote {
            return Ok(configuration
                .into_iter()
                .filter(|value| value.selection().provider_instance_id == provider)
                .skip(offset as usize)
                .take(limit as usize)
                .map(|value| value.fixed())
                .collect());
        }
        store
            .list_model_fixed_configs(provider, offset, limit)
            .await
            .map_err(|error| RuntimeError::from_store("list model parameters", error))
    }
}
