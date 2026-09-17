//! 服务商凭据与最后成功目录属于存储／执行边界；目录没有独立身份或生命周期。
use assistant_protocol::{
    DiscoveredModel, ModelCatalogDiagnosticCode, ProviderConnection, ProviderInstanceId,
    SecretValue,
};
use std::collections::BTreeSet;

const MAX_STORED_MODEL_CATALOG_MODELS: usize = 10_000;

/// 最近一次成功显式刷新的目录事实；时间与模型列表必须作为整体替换。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredModelCatalog {
    pub models: Vec<DiscoveredModel>,
    pub refreshed_at_ms: i64,
    pub connection_changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProvider {
    pub provider_instance_id: ProviderInstanceId,
    pub connection: ProviderConnection,
    pub api_key: SecretValue,
    pub model_catalog: Option<StoredModelCatalog>,
    /// 只表示持久快照无法解码；正常写入永远清除此诊断。
    pub model_catalog_diagnostic: Option<ModelCatalogDiagnosticCode>,
}

/// 目录联网完成后的存储 CAS 输入；凭据只用于比较，不会进入协议或日志。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderModelCatalogReplacement {
    pub provider_instance_id: ProviderInstanceId,
    pub expected_connection: ProviderConnection,
    pub expected_api_key: SecretValue,
    pub catalog: StoredModelCatalog,
}

/// Store Adapter 读写时共用的目录领域校验；持久值不得包含读取时派生的 configuration。
pub fn stored_model_catalog_is_valid(models: &[DiscoveredModel]) -> bool {
    if models.len() > MAX_STORED_MODEL_CATALOG_MODELS {
        return false;
    }
    let mut identifiers = BTreeSet::new();
    models.iter().all(|model| {
        !model.model_id.trim().is_empty()
            && model.model_id.len() <= 1024
            && !model.model_id.chars().any(char::is_control)
            && identifiers.insert(model.model_id.as_str())
            && model.configuration.is_none()
            && model
                .display_name
                .as_ref()
                .is_none_or(|name| name.len() <= 256 && !name.chars().any(char::is_control))
    })
}
