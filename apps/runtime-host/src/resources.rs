//! 正式 Host 的 Provider、Agent Factory、验证工具与 fail-closed Authorizer 装配。

use std::{env, sync::Arc};

use agent_context::ContextWindowEvaluator;
use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use agent_sdk::{Agent, AgentBuilder};
use agent_tools::{
    ResolvedToolBatch, ResolvedToolInvocation, Tool, ToolContext, ToolError, ToolExecuteFuture,
    ToolRegistry, ToolResolution, ToolSetSnapshot,
};
use agent_types::ToolName;
use assistant_runtime::{AgentFactoryError, SessionAgentFactory};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::ServeConfig;

const ECHO_TOOL_NAME: &str = "echo_text";
const CONTEXT_THRESHOLD: f64 = 0.8;

pub(crate) struct HostResources {
    pub(crate) factory: Arc<dyn SessionAgentFactory>,
    /// v0.8.0 尚未引入工作模式时，由 Host 显式提供的安全默认授权器。
    pub(crate) default_authorizer: Arc<dyn ToolAuthorizer>,
}

impl HostResources {
    pub(crate) fn from_config(config: &ServeConfig) -> Result<Self, ResourceError> {
        let credential = env::var(&config.api_key_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ResourceError::MissingCredential {
                variable: config.api_key_env.clone(),
            })?;
        let model: Arc<dyn ModelService> = Arc::new(
            OpenAiCompatibleService::new(
                config.base_url.clone(),
                BearerCredential::new(credential),
                config.model.clone(),
                config.context_window_tokens,
                Profile::deepseek(),
                TransportTimeouts::default(),
            )
            .map_err(|error| ResourceError::Provider(error.to_string()))?,
        );
        let mut registry = ToolRegistry::new();
        registry
            .register(EchoTextTool)
            .map_err(|error| ResourceError::Tool(error.to_string()))?;
        let factory = HostAgentFactory::new(model, registry.snapshot())?;
        Ok(Self {
            factory: Arc::new(factory),
            default_authorizer: Arc::new(EchoOnlyAuthorizer),
        })
    }
}

pub(crate) struct HostAgentFactory {
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    context_window: Arc<ContextWindowEvaluator>,
    system_prompt: SystemPromptSnapshot,
}

impl HostAgentFactory {
    fn new(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> Result<Self, ResourceError> {
        let context_window = ContextWindowEvaluator::new(CONTEXT_THRESHOLD)
            .map_err(|_| ResourceError::ContextThreshold)?;
        Ok(Self {
            model,
            tools,
            context_window: Arc::new(context_window),
            system_prompt: SystemPromptSnapshot::new(vec![
                "You are the EZ Assistant Runtime initialization demo. Use echo_text only when the user explicitly asks you to echo text.".to_owned(),
            ]),
        })
    }
}

impl SessionAgentFactory for HostAgentFactory {
    fn create_agent(&self) -> Result<Agent, AgentFactoryError> {
        AgentBuilder::new(
            self.model.clone(),
            self.system_prompt.clone(),
            self.context_window.clone(),
        )
        .tools(self.tools.clone())
        .build()
        .map_err(|source| AgentFactoryError::with_source("host agent build failed", source))
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
    #[error("required credential environment variable `{variable}` is missing or empty")]
    MissingCredential { variable: String },
    #[error("OpenAI-compatible Provider configuration is invalid: {0}")]
    Provider(String),
    #[error("echo_text tool registration failed: {0}")]
    Tool(String),
    #[error("static context threshold is invalid")]
    ContextThreshold,
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
