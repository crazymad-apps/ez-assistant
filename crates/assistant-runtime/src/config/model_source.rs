//! 配置来源适配：业务读取统一的服务商、默认选择和固定参数，不感知数据库或远程来源。

use assistant_protocol::{ModelFixedConfig, ModelSelection, ProviderInstanceId};

use super::{ConfigRegistry, ExternalModelConfiguration};
use crate::{RuntimeError, RuntimeResult, RuntimeStore};
use std::sync::Arc;

impl ConfigRegistry {
    pub(crate) fn effective_model_selection(
        &self,
        requested: Option<&ModelSelection>,
    ) -> RuntimeResult<Option<ModelSelection>> {
        let state = self.managed_models()?;
        Ok(requested.or_else(|| state.default_model()).cloned())
    }

    pub(crate) fn organization_configuration(
        &self,
        selection: &ModelSelection,
    ) -> RuntimeResult<Arc<ExternalModelConfiguration>> {
        if selection.provider_instance_id.as_str() == "managed" {
            return Err(RuntimeError::InvalidRequest {
                reason: "旧企业模型缺少可确认的服务商，请重新选择模型。",
            });
        }
        let state = self.managed_models()?;
        match state.organization_model(selection) {
            Some(Ok(configuration)) => Ok(configuration.clone()),
            Some(Err(reason)) => Err(RuntimeError::InvalidRequest { reason }),
            None => Err(RuntimeError::InvalidRequest {
                reason: "所选组织模型当前不可用，请刷新组织配置或重新选择。",
            }),
        }
    }

    pub(crate) async fn fixed_model(
        &self,
        selection: ModelSelection,
        store: &dyn RuntimeStore,
    ) -> RuntimeResult<Option<ModelFixedConfig>> {
        if selection.source == assistant_protocol::ModelSelectionSource::Organization {
            return Ok(Some(self.organization_configuration(&selection)?.fixed()));
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
        store
            .list_model_fixed_configs(provider, offset, limit)
            .await
            .map_err(|error| RuntimeError::from_store("list model parameters", error))
    }
}
