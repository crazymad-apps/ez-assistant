//! 正式 Host 的 Provider 工厂、System Prompt、工具与 fail-closed Authorizer 装配。

use std::sync::Arc;

use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use agent_tools::{
    ResolvedToolBatch, ResolvedToolInvocation, Tool, ToolContext, ToolError, ToolExecuteFuture,
    ToolRegistry, ToolResolution, ToolSetSnapshot,
};
use agent_types::ToolName;
use assistant_runtime::{
    ModelCompatibilityProfile, ModelServiceFactory, ModelServiceFactoryError,
    ModelServiceFactoryRequest, SystemPromptFactory, SystemPromptFactoryError,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ECHO_TOOL_NAME: &str = "echo_text";

pub(crate) struct HostResources {
    pub(crate) model_factory: Arc<dyn ModelServiceFactory>,
    pub(crate) system_prompt_factory: Arc<dyn SystemPromptFactory>,
    pub(crate) tools: ToolSetSnapshot,
    /// 尚未引入工作模式时，由 Host 显式提供的安全默认授权器。
    pub(crate) default_authorizer: Arc<dyn ToolAuthorizer>,
}

impl HostResources {
    pub(crate) fn new() -> Result<Self, ResourceError> {
        let mut registry = ToolRegistry::new();
        registry
            .register(EchoTextTool)
            .map_err(|error| ResourceError::Tool(error.to_string()))?;
        Ok(Self {
            model_factory: Arc::new(HostModelServiceFactory),
            system_prompt_factory: Arc::new(HostSystemPromptFactory),
            tools: registry.snapshot(),
            default_authorizer: Arc::new(EchoOnlyAuthorizer),
        })
    }
}

struct HostModelServiceFactory;

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

struct HostSystemPromptFactory;

impl SystemPromptFactory for HostSystemPromptFactory {
    fn create_system_prompt(&self) -> Result<SystemPromptSnapshot, SystemPromptFactoryError> {
        Ok(SystemPromptSnapshot::new(vec![
            "You are EZ Assistant. Use echo_text only when the user explicitly asks you to echo text."
                .to_owned(),
        ]))
    }
}

pub(crate) struct EchoOnlyAuthorizer;

impl ToolAuthorizer for EchoOnlyAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        let decision = if invocation.tool_name().as_str() == ECHO_TOOL_NAME {
            ToolAuthorization::Allow
        } else {
            ToolAuthorization::Deny {
                reason: "this Runtime initialization Host only permits echo_text".to_owned(),
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

#[derive(Clone, Copy)]
struct EchoTextTool;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
struct EchoTextInput {
    text: String,
}

#[derive(Clone, Debug, Serialize)]
struct EchoTextOutput {
    text: String,
}

impl Tool for EchoTextTool {
    type Input = EchoTextInput;
    type ResolvedInput = EchoTextInput;
    type Output = EchoTextOutput;

    fn name(&self) -> ToolName {
        ToolName::new(ECHO_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Return the provided text without accessing files, processes, network, or storage."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        Ok(ToolResolution::general(input))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(std::future::ready(Ok(EchoTextOutput { text: input.text })))
    }
}

#[derive(Debug, Error)]
pub(crate) enum ResourceError {
    #[error("echo_text tool registration failed: {0}")]
    Tool(String),
}

#[cfg(test)]
mod tests {
    use agent_tools::{Dispatcher, ResolvedBatchItemRef};
    use agent_types::{ToolCall, ToolCallId};
    use serde_json::json;

    use super::*;

    #[derive(Deserialize, JsonSchema, Serialize)]
    struct OtherInput {
        value: String,
    }

    struct OtherTool;

    impl Tool for OtherTool {
        type Input = OtherInput;
        type ResolvedInput = OtherInput;
        type Output = OtherInput;

        fn name(&self) -> ToolName {
            ToolName::new("future_tool").expect("tool name")
        }

        fn description(&self) -> String {
            "future tool".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    #[tokio::test]
    async fn authorizer_allows_only_echo_and_denies_future_registered_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(EchoTextTool).expect("echo");
        registry.register(OtherTool).expect("other");
        let snapshot = registry.snapshot();
        let calls = vec![
            ToolCall {
                id: ToolCallId::new("call-echo").expect("id"),
                name: ToolName::new(ECHO_TOOL_NAME).expect("name"),
                arguments: json!({"text": "hello"}),
            },
            ToolCall {
                id: ToolCallId::new("call-other").expect("id"),
                name: ToolName::new("future_tool").expect("name"),
                arguments: json!({"value": "hello"}),
            },
        ];
        let batch = Dispatcher::resolve_batch(&snapshot, &calls);
        let authorizer = EchoOnlyAuthorizer;
        let mut decisions = Vec::new();
        for item in batch.iter() {
            let ResolvedBatchItemRef::Valid(invocation) = item else {
                panic!("fixture resolves");
            };
            decisions.push(authorizer.authorize(invocation, &batch).await);
        }
        assert_eq!(decisions[0], ToolAuthorization::Allow);
        assert!(matches!(decisions[1], ToolAuthorization::Deny { .. }));
    }
}
