//! 固定 `call_mcp_tool` 网关及其 Runtime 私有授权事实。

use std::{collections::BTreeSet, sync::Arc};

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::{ToolName, ToolResultContent, ToolResultStatus};
use assistant_protocol::{McpServerKey, McpToolIdentity};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::registry::{McpCallFailure, McpCallFailureKind, ResolvedMcpInvocation};
use super::{McpImageMaterializer, McpRegistry};

pub(crate) const CALL_MCP_TOOL_NAME: &str = "call_mcp_tool";
const MAX_ARGUMENT_BYTES: usize = 256 * 1024;

/// 本次 Run 允许网关引用的稳定 Server key 集合。它只冻结披露范围，不复制 Tool 目录。
#[derive(Clone, Debug)]
pub(crate) struct McpDisclosureScope {
    server_keys: BTreeSet<McpServerKey>,
}

impl McpDisclosureScope {
    pub(crate) fn new(server_keys: BTreeSet<McpServerKey>) -> Self {
        Self { server_keys }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.server_keys.is_empty()
    }

    pub(crate) fn contains(&self, server_key: &McpServerKey) -> bool {
        self.server_keys.contains(server_key)
    }
}

/// 权限、审批和审计只读取实际 MCP 身份，不按固定网关名称授权。
#[derive(Clone, Debug)]
pub(crate) struct McpAuthorizationFacts {
    pub(crate) invocation: ResolvedMcpInvocation,
}

impl McpAuthorizationFacts {
    pub(crate) fn identity(&self) -> McpToolIdentity {
        McpToolIdentity {
            server_key: self.invocation.server_key.clone(),
            server_display_name: self.invocation.server_display_name.clone(),
            tool_name: self.invocation.tool_name.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallMcpToolInput {
    server: String,
    tool: String,
    arguments: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CallMcpToolOutput {
    #[serde(skip)]
    content: ToolResultContent,
    #[serde(skip)]
    is_error: bool,
}

pub(crate) struct CallMcpTool {
    registry: Arc<McpRegistry>,
    scope: McpDisclosureScope,
    session_tool_image_directory: Option<String>,
    image_materializer: Arc<dyn McpImageMaterializer>,
}

impl CallMcpTool {
    pub(crate) fn new(
        registry: Arc<McpRegistry>,
        scope: McpDisclosureScope,
        session_tool_image_directory: Option<String>,
        image_materializer: Arc<dyn McpImageMaterializer>,
    ) -> Self {
        Self {
            registry,
            scope,
            session_tool_image_directory,
            image_materializer,
        }
    }
}

impl Tool for CallMcpTool {
    type Input = CallMcpToolInput;
    type ResolvedInput = ResolvedMcpInvocation;
    type Output = CallMcpToolOutput;

    fn name(&self) -> ToolName {
        ToolName::new(CALL_MCP_TOOL_NAME).expect("fixed MCP gateway name is valid")
    }

    fn description(&self) -> String {
        "Call one tool from an MCP server already disclosed for this run. Use the exact server key and raw tool name, and pass arguments matching the disclosed input schema."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let server_key = McpServerKey::new(input.server)
            .map_err(|_| ToolError::invalid_input("mcp_server_invalid"))?;
        if !self.scope.contains(&server_key) {
            return Err(ToolError::invalid_input("mcp_server_outside_scope"));
        }
        if input.tool.is_empty() || input.tool.len() > 128 {
            return Err(ToolError::invalid_input("mcp_tool_invalid"));
        }
        let arguments = input.arguments.into_iter().collect::<Map<_, _>>();
        if serde_json::to_vec(&arguments).map_or(true, |encoded| encoded.len() > MAX_ARGUMENT_BYTES)
        {
            return Err(ToolError::invalid_input("mcp_arguments_too_large"));
        }
        let invocation = self
            .registry
            .resolve_identity(&server_key, &input.tool, arguments)
            .map_err(resolve_failure)?;
        let semantic_arguments = json!({
            "server": invocation.server_key.as_str(),
            "tool": &invocation.tool_name,
            "arguments": &invocation.arguments,
        });
        Ok(ToolResolution::with_facts(
            invocation.clone(),
            McpAuthorizationFacts { invocation },
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let projection = self
                .registry
                .execute(
                    &input,
                    self.session_tool_image_directory.as_deref(),
                    self.image_materializer.as_ref(),
                    context.cancellation,
                )
                .await
                .map_err(execution_failure)?;
            debug_assert!(projection.remote_may_have_executed);
            Ok(CallMcpToolOutput {
                content: projection.content,
                is_error: projection.is_error,
            })
        })
    }

    fn output_status(output: &Self::Output) -> ToolResultStatus {
        if output.is_error {
            ToolResultStatus::Error
        } else {
            ToolResultStatus::Success
        }
    }

    fn encode_output(output: Self::Output) -> Result<ToolResultContent, String> {
        Ok(output.content)
    }
}

fn resolve_failure(failure: McpCallFailure) -> ToolError {
    ToolError::invalid_input(failure_code(failure.kind))
}

fn execution_failure(failure: McpCallFailure) -> ToolError {
    let code = failure_code(failure.kind);
    ToolError::execution_with_details(
        code,
        json!({
            "code": code,
            "instance_path": failure.instance_path,
            "keyword": failure.keyword,
            "remote_may_have_executed": failure.remote_may_have_executed,
        }),
    )
}

pub(crate) fn failure_code(kind: McpCallFailureKind) -> &'static str {
    match kind {
        McpCallFailureKind::ServerUnavailable => "mcp_server_unavailable",
        McpCallFailureKind::CatalogChanged => "mcp_catalog_changed",
        McpCallFailureKind::InvalidInput => "mcp_invalid_arguments",
        McpCallFailureKind::RequestFailed => "mcp_request_failed",
        McpCallFailureKind::UnsupportedResult => "mcp_result_unsupported",
        McpCallFailureKind::ResultLimit => "mcp_result_too_large",
        McpCallFailureKind::Cancelled => "mcp_call_cancelled",
    }
}
