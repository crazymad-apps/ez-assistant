//! Runtime 单一模型设置快照。网络不持配置写门；短提交后替换对应 Provider Arc 并广播配置变化。

use super::AssistantRuntime;
use crate::{
    ModelDiscoveryRequest, ModelParameters, ProviderModelCatalogReplacement, RuntimeError,
    RuntimeResult, StoreErrorKind, StoredModelCatalog, StoredProvider,
    stored_model_catalog_is_valid,
};
use assistant_protocol::{
    CreateProviderRequest, DiscoveredModel, GetModelConfigurationRequest,
    ListFixedModelConfigsRequest, ModelConfigOrigin, ModelConfigurationDetail,
    ModelConfigurationSource, ModelFixedConfig, ModelSelection, ModelSettings, ProviderConnection,
    ProviderCredentialChange, ProviderInstanceId, ProviderModelCatalogSnapshot, ProviderSummary,
    RuntimeEvent, SaveModelFixedConfigRequest, SecretValue, UpdateProviderRequest,
};
use std::{
    sync::{Arc, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use crate::config::ManagedModels;

mod catalog;
mod fixed_config;
mod provider;
mod selection;
mod validation;

use catalog::catalog_model;
pub(super) use validation::validate_selection;
use validation::{
    credential, validate_connection, validate_provider_credential, validate_provider_parameters,
};

impl AssistantRuntime {
    pub(super) fn provider(&self, id: &ProviderInstanceId) -> RuntimeResult<Arc<StoredProvider>> {
        self.managed_models()?
            .providers
            .get(id)
            .cloned()
            .ok_or_else(|| invalid("服务商不存在或已删除，请重新选择。"))
    }
    pub(super) fn ensure_provider_current(
        &self,
        captured: &Arc<StoredProvider>,
    ) -> RuntimeResult<()> {
        if self
            .managed_models()?
            .providers
            .get(&captured.provider_instance_id)
            .is_some_and(|current| Arc::ptr_eq(current, captured))
        {
            Ok(())
        } else {
            Err(RuntimeError::ConfigurationConflict)
        }
    }
    fn managed_models(&self) -> RuntimeResult<RwLockReadGuard<'_, ManagedModels>> {
        self.config_registry.managed_models()
    }
    fn managed_models_mut(&self) -> RuntimeResult<RwLockWriteGuard<'_, ManagedModels>> {
        self.config_registry.managed_models_mut()
    }
}

fn invalid(reason: &'static str) -> RuntimeError {
    RuntimeError::InvalidRequest { reason }
}
