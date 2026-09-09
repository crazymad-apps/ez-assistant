//! 服务商凭据属于存储／执行边界，不作为客户端查询响应。
use assistant_protocol::{ProviderConnection, ProviderInstanceId, SecretValue};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProvider {
    pub provider_instance_id: ProviderInstanceId,
    pub connection: ProviderConnection,
    pub api_key: SecretValue,
}
