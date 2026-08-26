//! Provider 可见的三个主控工具契约。

use std::sync::Arc;

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolRegistry, ToolResolution, ToolSetSnapshot,
};
use agent_types::ToolName;
use assistant_protocol::{RunId, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ControllerToolCoordinator, DeliveryReceipt, ManagedSession};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ControllerAuthorizationFacts;

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct ListManagedSessionsInput {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ListManagedSessionsOutput {
    sessions: Vec<ManagedSession>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct SetSessionProxyInput {
    session_id: String,
    enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SetSessionProxyOutput {
    session_id: String,
    enabled: bool,
    changed: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct SendSessionMessageInput {
    session_id: String,
    message: String,
}

struct ListManagedSessionsTool {
    coordinator: Arc<ControllerToolCoordinator>,
    controller_session_id: SessionId,
}

struct SetSessionProxyTool {
    coordinator: Arc<ControllerToolCoordinator>,
    controller_session_id: SessionId,
}

struct SendSessionMessageTool {
    coordinator: Arc<ControllerToolCoordinator>,
    controller_session_id: SessionId,
    controller_run_id: RunId,
}

pub(crate) fn controller_tool_set(
    coordinator: Arc<ControllerToolCoordinator>,
    controller_session_id: SessionId,
    controller_run_id: RunId,
) -> Result<ToolSetSnapshot, agent_tools::RegisterToolError> {
    let mut registry = ToolRegistry::new();
    registry.register(ListManagedSessionsTool {
        coordinator: coordinator.clone(),
        controller_session_id: controller_session_id.clone(),
    })?;
    registry.register(SetSessionProxyTool {
        coordinator: coordinator.clone(),
        controller_session_id: controller_session_id.clone(),
    })?;
    registry.register(SendSessionMessageTool {
        coordinator,
        controller_session_id,
        controller_run_id,
    })?;
    Ok(registry.snapshot())
}

impl Tool for ListManagedSessionsTool {
    type Input = ListManagedSessionsInput;
    type ResolvedInput = ListManagedSessionsInput;
    type Output = ListManagedSessionsOutput;

    fn name(&self) -> ToolName {
        ToolName::new("list_managed_sessions").expect("static tool name")
    }

    fn description(&self) -> String {
        "List active standard sessions and whether this controller currently proxies them. Conversation contents are not returned.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        Ok(ToolResolution::with_facts(
            input,
            ControllerAuthorizationFacts,
            serde_json::json!({}),
        ))
    }

    fn execute<'a>(
        &'a self,
        _input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.coordinator
                .list_managed_sessions(&self.controller_session_id)
                .map(|sessions| ListManagedSessionsOutput { sessions })
                .map_err(|_| ToolError::execution("managed sessions are unavailable"))
        })
    }
}

impl Tool for SetSessionProxyTool {
    type Input = SetSessionProxyInput;
    type ResolvedInput = (SessionId, bool);
    type Output = SetSessionProxyOutput;

    fn name(&self) -> ToolName {
        ToolName::new("set_session_proxy").expect("static tool name")
    }

    fn description(&self) -> String {
        "Explicitly enable or disable controller proxy mode for one standard session. This changes state immediately and does not wait for its input queue.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let session_id = SessionId::new(input.session_id)
            .map_err(|_| ToolError::invalid_input("session_id is invalid"))?;
        Ok(ToolResolution::with_facts(
            (session_id, input.enabled),
            ControllerAuthorizationFacts,
            serde_json::json!({"session_id": "validated", "enabled": input.enabled}),
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let (session_id, enabled) = input;
            let changed = self
                .coordinator
                .set_proxy(&self.controller_session_id, &session_id, enabled)
                .await
                .map_err(|_| ToolError::execution("session proxy could not be changed"))?;
            Ok(SetSessionProxyOutput {
                session_id: session_id.as_str().to_owned(),
                enabled,
                changed,
            })
        })
    }
}

impl Tool for SendSessionMessageTool {
    type Input = SendSessionMessageInput;
    type ResolvedInput = (SessionId, String);
    type Output = DeliveryReceipt;

    fn name(&self) -> ToolName {
        ToolName::new("send_session_message").expect("static tool name")
    }

    fn description(&self) -> String {
        "Deliver one text task to a session currently proxied by this controller. The target rejects delivery while it still has queued input and executes with its own model and permissions.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let session_id = SessionId::new(input.session_id)
            .map_err(|_| ToolError::invalid_input("session_id is invalid"))?;
        let semantic =
            serde_json::json!({"session_id": session_id.as_str(), "message": &input.message});
        Ok(ToolResolution::with_facts(
            (session_id, input.message),
            ControllerAuthorizationFacts,
            semantic,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let call_id = context.call_id().ok_or_else(|| {
                ToolError::execution("controller tool call identity is unavailable")
            })?;
            let call_id = assistant_protocol::ToolCallId::new(call_id.as_str().to_owned())
                .map_err(|_| ToolError::execution("controller tool call identity is invalid"))?;
            self.coordinator
                .deliver(
                    &self.controller_session_id,
                    &self.controller_run_id,
                    &call_id,
                    &input.0,
                    input.1,
                )
                .await
                .map_err(|_| ToolError::execution("session message could not be delivered"))
        })
    }
}
