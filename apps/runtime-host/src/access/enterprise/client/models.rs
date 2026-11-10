//! 校验中心模型集合的主体和代理地址，再投影为 Runtime 的组织配置。

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    ModelFeatureSupport, ModelParameters, ModelSelection, ModelSelectionSource, ProviderConnection,
    ProviderInstanceId, ProviderProtocolPreference, ProviderType, SecretValue,
};
use assistant_runtime::{ExternalModelConfiguration, ExternalModelSet};
use serde::Deserialize;

use super::{CenterClient, CenterError, decode, decode_with_limit, valid_center_id};

// 模型集合携带每项能力参数，保留独立有界预算，不放宽身份等其他响应。
const MODEL_CONFIGURATION_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct ConfigurationResponse {
    center_id: String,
    organization_id: String,
    allow_member_selection: bool,
    default_model: Option<Selection>,
    providers: Vec<Provider>,
}

#[derive(Deserialize)]
struct Selection {
    provider_instance_id: ProviderInstanceId,
    model_id: String,
}

#[derive(Deserialize)]
struct Provider {
    provider_instance_id: ProviderInstanceId,
    endpoint: String,
    provider_type: Option<ProviderType>,
    provider_display_name: Option<String>,
    protocol: Option<Protocol>,
    server_history_enabled: bool,
    server_history_support: ModelFeatureSupport,
    models: Vec<Model>,
}

#[derive(Clone, Copy, Deserialize)]
enum Protocol {
    #[serde(rename = "open_ai_responses")]
    Responses,
    #[serde(rename = "open_ai_chat_completions")]
    ChatCompletions,
}

#[derive(Deserialize)]
struct Model {
    model_id: String,
    #[serde(flatten)]
    state: ModelState,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ModelState {
    Ready {
        parameters: Box<ModelParameters>,
        server_history_binding: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

impl CenterClient {
    pub(in crate::access::enterprise) async fn model_configuration(
        &self,
        token: &SecretValue,
        expected_center: &str,
        expected_organization: &str,
    ) -> Result<ExternalModelSet, CenterError> {
        #[derive(Deserialize)]
        struct Info {
            protocol_version: u32,
            min_protocol_version: u32,
            #[serde(default)]
            capabilities: Vec<String>,
        }
        let info: Info = decode(self.client.get(self.endpoint("info"))).await?;
        super::check_protocol_range(info.min_protocol_version, info.protocol_version)?;
        if !["managed_models", "llm_proxy"]
            .iter()
            .all(|required| info.capabilities.iter().any(|value| value == required))
        {
            return Err(CenterError::ModelCapabilityMissing);
        }
        let response: ConfigurationResponse = decode_with_limit(
            self.client
                .get(self.endpoint("runtime/model-configuration"))
                .bearer_auth(token.expose()),
            MODEL_CONFIGURATION_BYTES,
        )
        .await?;
        self.compile_model_configuration(response, expected_center, expected_organization)
    }

    fn compile_model_configuration(
        &self,
        response: ConfigurationResponse,
        expected_center: &str,
        expected_organization: &str,
    ) -> Result<ExternalModelSet, CenterError> {
        if response.center_id != expected_center
            || response.organization_id != expected_organization
        {
            return Err(CenterError::IdentityMismatch);
        }
        let origin = url::Url::parse(&self.url)
            .map_err(|_| CenterError::Unavailable)?
            .origin();
        let fetched_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_millis()).ok())
            .ok_or(CenterError::Unavailable)?;
        let mut providers = std::collections::BTreeSet::new();
        let mut models = BTreeMap::new();
        for provider in response.providers {
            let id = provider.provider_instance_id.clone();
            let endpoint =
                url::Url::parse(&provider.endpoint).map_err(|_| CenterError::Unavailable)?;
            // 精确核对 URL 中的服务商身份；凭据只能发往当前登录中心的该服务商路由。
            if !valid_center_id(id.as_str())
                || endpoint.origin() != origin
                || endpoint.path() != format!("/api/llm/providers/{}/v1", id.as_str())
                || endpoint.query().is_some()
                || endpoint.fragment().is_some()
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || !providers.insert(id.clone())
                || provider.models.is_empty()
            {
                return Err(CenterError::Unavailable);
            }
            let connection = provider.connection();
            for model in provider.models {
                let selection = ModelSelection {
                    source: ModelSelectionSource::Organization,
                    provider_instance_id: id.clone(),
                    model_id: model.model_id,
                };
                let entry = match model.state {
                    ModelState::Ready {
                        parameters,
                        server_history_binding,
                    } => {
                        let binding_valid = server_history_binding.as_ref().is_none_or(|binding| {
                            binding.len() == 64
                                && binding.bytes().all(|byte| byte.is_ascii_hexdigit())
                        });
                        connection
                            .as_ref()
                            .filter(|_| binding_valid)
                            .and_then(|connection| {
                                ExternalModelConfiguration::new(
                                    id.clone(),
                                    connection.clone(),
                                    selection.model_id.clone(),
                                    *parameters,
                                    fetched_at_ms,
                                    server_history_binding,
                                )
                                .ok()
                            })
                            .map(Arc::new)
                            .ok_or("组织模型参数无效，请联系管理员。")
                    }
                    ModelState::Unavailable { reason } => Err(match reason.as_str() {
                        "provider_deleted" => "组织模型的服务商已删除。",
                        "model_absent" => "组织模型已不在服务商目录中。",
                        "invalid_parameters" => "组织模型参数不完整。",
                        _ => "组织模型当前不可用。",
                    }),
                };
                if selection.model_id.trim().is_empty()
                    || selection.model_id.len() > 1024
                    || selection.model_id.chars().any(char::is_control)
                    || models.insert(selection, entry).is_some()
                {
                    return Err(CenterError::Unavailable);
                }
            }
        }
        let default_model = response.default_model.map(|selection| ModelSelection {
            source: ModelSelectionSource::Organization,
            provider_instance_id: selection.provider_instance_id,
            model_id: selection.model_id,
        });
        if default_model
            .as_ref()
            .map_or(!models.is_empty(), |default| !models.contains_key(default))
            || (!response.allow_member_selection && models.len() > 1)
        {
            return Err(CenterError::Unavailable);
        }
        Ok(ExternalModelSet {
            default_model,
            allow_member_selection: response.allow_member_selection,
            models,
        })
    }
}

impl Provider {
    fn connection(&self) -> Option<ProviderConnection> {
        let protocol = self.protocol?;
        let provider_type = self.provider_type?;
        if self.server_history_enabled
            && (!matches!(protocol, Protocol::Responses)
                || self.server_history_support != ModelFeatureSupport::Supported)
        {
            return None;
        }
        Some(ProviderConnection {
            provider_type,
            display_name: self.provider_display_name.clone()?,
            endpoint: self.endpoint.clone(),
            protocol_preference: match protocol {
                Protocol::Responses => ProviderProtocolPreference::Responses,
                Protocol::ChatCompletions => ProviderProtocolPreference::ChatCompletions,
            },
            discovery_format: provider_type.discovery_format(),
            models_path: String::new(),
            server_history_enabled: self.server_history_enabled,
            // 代理地址只用于传输，续接能力由中心根据真实上游判定。
            server_history_support_override: match self.server_history_support {
                ModelFeatureSupport::Unknown => None,
                support => Some(support),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_model_remains_unavailable_without_replacing_the_rest_of_the_set() {
        const CENTER: &str = "01234567-89ab-4cde-8f01-23456789abcd";
        const ORGANIZATION: &str = "11234567-89ab-4cde-8f01-23456789abcd";
        let client = CenterClient::new("https://center.test".into()).unwrap();
        let parameters = ModelParameters {
            context_window_tokens: assistant_protocol::ModelTokenLimit::Known(
                8192.try_into().unwrap(),
            ),
            max_output_tokens: assistant_protocol::ModelTokenLimit::Known(1024.try_into().unwrap()),
            streaming: ModelFeatureSupport::Supported,
            ..Default::default()
        };
        let wire = serde_json::json!({
            "center_id": CENTER, "organization_id": ORGANIZATION, "allow_member_selection": true,
            "default_model": { "provider_instance_id": CENTER, "model_id": "ready" },
            "providers": [{
                "provider_instance_id": CENTER,
                "endpoint": format!("https://center.test/api/llm/providers/{CENTER}/v1"),
                "provider_type": "openai", "provider_display_name": "fixture",
                "protocol": "open_ai_responses", "server_history_enabled": false, "server_history_support": "unknown",
                "models": [
                    { "model_id": "ready", "state": "ready", "parameters": parameters, "server_history_binding": null },
                    { "model_id": "removed", "state": "unavailable", "reason": "model_absent" },
                ],
            }],
        });
        let parse = |input| serde_json::from_value(input).unwrap();
        let set = client
            .compile_model_configuration(parse(wire.clone()), CENTER, ORGANIZATION)
            .unwrap();
        assert_eq!(set.models.len(), 2);
        assert!(
            set.models
                .get(set.default_model.as_ref().unwrap())
                .unwrap()
                .is_ok()
        );
        assert!(set.models.values().any(Result::is_err));
        assert!(matches!(
            client.compile_model_configuration(parse(wire.clone()), CENTER, CENTER),
            Err(CenterError::IdentityMismatch)
        ));
        for endpoint in [
            format!("https://other.test/api/llm/providers/{CENTER}/v1"),
            format!("https://center.test/api/llm/providers/{ORGANIZATION}/v1"),
        ] {
            let mut mismatched = wire.clone();
            mismatched["providers"][0]["endpoint"] = endpoint.into();
            assert!(matches!(
                client.compile_model_configuration(parse(mismatched), CENTER, ORGANIZATION),
                Err(CenterError::Unavailable)
            ));
        }
    }
}
