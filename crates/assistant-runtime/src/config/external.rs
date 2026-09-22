//! 受信宿主提供的进程内模型配置；不含凭据，不读写本地模型存储。

use std::sync::Arc;

use agent_model::GenerationConfig;
use assistant_protocol::{
    ModelParameters, ModelSelection, ProviderConnection, ProviderInstanceId, SecretValue,
};

use super::managed_model::compile_managed_model;
use crate::{RuntimeError, RuntimeResult, StoredProvider};

/// 用户域装配时固定的模型权威来源，与产品模式和身份无关。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelSource {
    #[default]
    Local,
    External,
}

/// 已校验的外部模型配置，仅由受信宿主通过进程内 API 发布。
/// 不实现序列化；Arc 身份用于拒绝旧配置的晚到结果，不是持久版本号。
pub struct ExternalModelConfiguration {
    provider: Arc<StoredProvider>,
    selection: ModelSelection,
    parameters: ModelParameters,
}

impl ExternalModelConfiguration {
    /// 校验连接、参数和执行能力；宿主须在调用前核验连接属于可信来源。
    ///
    /// # Errors
    /// 无效连接、模型标识或不完整执行参数返回配置错误。
    pub fn new(
        connection: ProviderConnection,
        model_id: String,
        parameters: ModelParameters,
        fetched_at_ms: i64,
    ) -> RuntimeResult<Self> {
        super::validation::validate_provider_parameters(&connection, &parameters)?;
        let selection = ModelSelection {
            provider_instance_id: ProviderInstanceId::new("managed".to_owned())
                .map_err(|_| RuntimeError::ConfigurationUnavailable)?,
            model_id,
        };
        super::validation::validate_selection(&selection)?;
        let provider = Arc::new(StoredProvider {
            provider_instance_id: selection.provider_instance_id.clone(),
            connection,
            api_key: SecretValue::new(String::new()),
            model_catalog: Some(crate::StoredModelCatalog {
                models: vec![assistant_protocol::DiscoveredModel {
                    model_id: selection.model_id.clone(),
                    display_name: None,
                    metadata: parameters.clone(),
                    configuration: None,
                }],
                refreshed_at_ms: fetched_at_ms,
                connection_changed: false,
            }),
            model_catalog_diagnostic: None,
        });
        compile_managed_model(
            &provider,
            &selection,
            &parameters,
            &GenerationConfig::default(),
        )?;
        let result = Self {
            provider,
            selection,
            parameters,
        };
        Ok(result)
    }

    pub(crate) fn provider(&self) -> Arc<StoredProvider> {
        self.provider.clone()
    }

    pub(crate) fn fixed(&self) -> assistant_protocol::ModelFixedConfig {
        assistant_protocol::ModelFixedConfig {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: self.selection.clone(),
            parameters: self.parameters.clone(),
            updated_at_ms: self
                .provider
                .model_catalog
                .as_ref()
                .map_or(0, |catalog| catalog.refreshed_at_ms),
        }
    }

    pub fn model_id(&self) -> &str {
        &self.selection.model_id
    }

    pub fn parameters(&self) -> &ModelParameters {
        &self.parameters
    }

    pub(crate) fn selection(&self) -> &ModelSelection {
        &self.selection
    }
}
