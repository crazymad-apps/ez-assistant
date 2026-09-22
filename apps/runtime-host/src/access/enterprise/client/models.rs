//! 中心私有配置到已校验 Runtime 外部配置的适配，不泄漏原始错误体或 endpoint。

use std::sync::Arc;

use assistant_protocol::{
    ModelParameters, ProviderConnection, ProviderProtocolPreference, ProviderType, SecretValue,
};
use assistant_runtime::ExternalModelConfiguration;
use serde::Deserialize;

use super::{CenterClient, CenterError, decode, valid_center_id};

pub(crate) enum ModelConfiguration {
    Ready(Arc<ExternalModelConfiguration>),
    Unavailable(&'static str),
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ConfigurationResponse {
    Ready {
        center_id: String,
        endpoint: String,
        provider_type: ProviderType,
        provider_display_name: String,
        protocol: Protocol,
        model_id: String,
        parameters: ModelParameters,
    },
    Unavailable {
        center_id: String,
        reason: String,
    },
}

#[derive(Deserialize)]
enum Protocol {
    #[serde(rename = "open_ai_responses")]
    Responses,
    #[serde(rename = "open_ai_chat_completions")]
    ChatCompletions,
}

impl CenterClient {
    pub(in crate::access::enterprise) async fn model_configuration(
        &self,
        token: &SecretValue,
        expected_center: &str,
    ) -> Result<ModelConfiguration, CenterError> {
        #[derive(Deserialize)]
        struct Info {
            protocol_version: u32,
            min_protocol_version: u32,
            #[serde(default)]
            capabilities: Vec<String>,
        }
        let info: Info = decode(self.client.get(self.endpoint("info"))).await?;
        if info.min_protocol_version == 0
            || info.min_protocol_version > info.protocol_version
            || !(info.min_protocol_version..=info.protocol_version).contains(&1)
        {
            return Err(CenterError::ProtocolIncompatible);
        }
        if !["managed_models", "llm_proxy"]
            .iter()
            .all(|required| info.capabilities.iter().any(|value| value == required))
        {
            return Err(CenterError::ModelCapabilityMissing);
        }
        let response: ConfigurationResponse = decode(
            self.client
                .get(self.endpoint("runtime/model-configuration"))
                .bearer_auth(token.expose()),
        )
        .await?;
        self.compile_model_configuration(response, expected_center)
    }

    fn compile_model_configuration(
        &self,
        response: ConfigurationResponse,
        expected_center: &str,
    ) -> Result<ModelConfiguration, CenterError> {
        let identity = match &response {
            ConfigurationResponse::Ready { center_id, .. }
            | ConfigurationResponse::Unavailable { center_id, .. } => center_id,
        };
        if identity != expected_center {
            return Err(CenterError::IdentityMismatch);
        }
        match response {
            ConfigurationResponse::Unavailable { reason, .. } => {
                let reason = match reason.as_str() {
                    "not_selected" => "管理员尚未配置默认模型。",
                    "provider_deleted" => "默认模型的服务商已删除。",
                    "model_absent" => "默认模型已不在服务商目录中。",
                    "invalid_parameters" => "默认模型参数不完整。",
                    _ => return Err(CenterError::Unavailable),
                };
                Ok(ModelConfiguration::Unavailable(reason))
            }
            ConfigurationResponse::Ready {
                endpoint,
                provider_type,
                provider_display_name,
                protocol,
                model_id,
                parameters,
                ..
            } => {
                let configured =
                    url::Url::parse(&self.url).map_err(|_| CenterError::Unavailable)?;
                let endpoint_url =
                    url::Url::parse(&endpoint).map_err(|_| CenterError::Unavailable)?;
                let provider = endpoint_url
                    .path()
                    .strip_prefix("/api/llm/providers/")
                    .and_then(|path| path.strip_suffix("/v1"));
                if endpoint_url.origin() != configured.origin()
                    || !provider.is_some_and(valid_center_id)
                    || endpoint_url.query().is_some()
                    || endpoint_url.fragment().is_some()
                    || !endpoint_url.username().is_empty()
                    || endpoint_url.password().is_some()
                {
                    return Err(CenterError::Unavailable);
                }
                let connection = ProviderConnection {
                    display_name: provider_display_name,
                    discovery_format: provider_type.discovery_format(),
                    provider_type,
                    endpoint,
                    protocol_preference: match protocol {
                        Protocol::Responses => ProviderProtocolPreference::Responses,
                        Protocol::ChatCompletions => ProviderProtocolPreference::ChatCompletions,
                    },
                    models_path: String::new(),
                };
                let fetched_at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|value| i64::try_from(value.as_millis()).ok())
                    .ok_or(CenterError::Unavailable)?;
                ExternalModelConfiguration::new(connection, model_id, parameters, fetched_at_ms)
                    .map(|value| ModelConfiguration::Ready(Arc::new(value)))
                    .map_err(|_| CenterError::Unavailable)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_configuration_requires_center_identity_and_exact_trusted_proxy_origin() {
        const CENTER: &str = "01234567-89ab-4cde-8f01-23456789abcd";
        let client = CenterClient::new("https://center.test".to_owned()).unwrap();
        let parameters = ModelParameters {
            context_window_tokens: assistant_protocol::ModelTokenLimit::Known(
                8192.try_into().unwrap(),
            ),
            max_output_tokens: assistant_protocol::ModelTokenLimit::Known(1024.try_into().unwrap()),
            streaming: assistant_protocol::ModelFeatureSupport::Supported,
            ..Default::default()
        };
        let configuration = |endpoint: String, center_id: &str| ConfigurationResponse::Ready {
            center_id: center_id.to_owned(),
            endpoint,
            provider_type: ProviderType::Openai,
            provider_display_name: "测试服务商".to_owned(),
            protocol: Protocol::Responses,
            model_id: "test-model".to_owned(),
            parameters: parameters.clone(),
        };
        let path = format!("/api/llm/providers/{CENTER}/v1");
        assert!(
            client
                .compile_model_configuration(
                    configuration(format!("https://center.test{path}"), CENTER),
                    CENTER
                )
                .is_ok()
        );
        for endpoint in [
            format!("https://other.test{path}"),
            format!("https://center.test:8443{path}"),
            format!("https://user@center.test{path}"),
            format!("https://center.test{path}?secret=x"),
            format!("https://center.test{path}#fragment"),
            "https://center.test/v1".to_owned(),
        ] {
            assert!(
                client
                    .compile_model_configuration(configuration(endpoint, CENTER), CENTER)
                    .is_err()
            );
        }
        assert!(matches!(
            client.compile_model_configuration(
                configuration(format!("https://center.test{path}"), "wrong-center"),
                CENTER
            ),
            Err(CenterError::IdentityMismatch)
        ));
    }
}
