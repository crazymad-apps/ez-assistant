//! 已编译 Runtime 模型配置到 OpenAI-compatible Adapter 的 Host 装配。

use std::sync::Arc;

use agent_model::ModelService;
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use assistant_runtime::{
    ModelCompatibilityProfile, ModelServiceFactory, ModelServiceFactoryError,
    ModelServiceFactoryRequest,
};

pub(super) struct HostModelServiceFactory;

impl ModelServiceFactory for HostModelServiceFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError> {
        let profile = match request.profile {
            ModelCompatibilityProfile::DeepSeek => Profile::deepseek(),
            ModelCompatibilityProfile::Standard => {
                Profile::openai_compatible(request.provider.clone())
            }
        };
        let service = OpenAiCompatibleService::new(
            request.endpoint,
            BearerCredential::new(request.api_key.to_owned()),
            request.model,
            request.context_window_tokens,
            profile,
            TransportTimeouts {
                connect: request.connect_timeout,
                request: request.request_timeout,
            },
        )
        .map_err(|source| {
            ModelServiceFactoryError::with_source("model service could not be created", source)
        })?;
        Ok(Arc::new(service))
    }
}
