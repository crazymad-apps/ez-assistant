//! 父 Run 的 MCP 管理工具；只有窄配置句柄，不能选择用户域、读取秘密或修改受管启动定义。

use std::{collections::BTreeMap, sync::Arc};

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolRegistry, ToolResolution, ToolSetSnapshot,
};
use agent_types::ToolName;
use assistant_protocol::{self as protocol, McpServerSnapshot, McpServerSource, McpServerTarget};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::McpManagement;
use crate::{RuntimeError, RuntimeResult, session::SessionController};

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", content = "name", rename_all = "snake_case")]
enum Source {
    User,
    Plugin(String),
    Builtin,
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Target {
    source: Source,
    server_key: String,
}

impl Target {
    fn resolve(&self) -> Result<McpServerTarget, ToolError> {
        let source = match &self.source {
            Source::User => McpServerSource::User,
            Source::Builtin => McpServerSource::Builtin,
            Source::Plugin(name) if !name.is_empty() && name.len() <= 128 => {
                McpServerSource::Plugin(name.clone())
            }
            _ => return Err(ToolError::invalid_input("invalid plugin name")),
        };
        Ok(McpServerTarget {
            source,
            server_key: protocol::McpServerKey::new(self.server_key.clone())
                .map_err(|_| ToolError::invalid_input("invalid server_key"))?,
        })
    }
}

#[derive(Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExpectedTarget {
    target: Target,
    expected_revision: String,
    /// 独立 user 服务使用空字符串；受管服务必须使用查询得到的摘要。
    expected_definition_digest: String,
}

#[derive(Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetInput {
    target: Option<Target>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(
    tag = "mode",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SecretChange {
    Keep,
    Replace(String),
    Remove,
}

impl From<SecretChange> for protocol::McpSecretChange {
    fn from(value: SecretChange) -> Self {
        match value {
            SecretChange::Keep => Self::Keep,
            SecretChange::Replace(value) => Self::Replace(protocol::SecretValue::new(value)),
            SecretChange::Remove => Self::Remove,
        }
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Patch {
        enabled: Option<bool>,
        #[serde(default)]
        environment: BTreeMap<String, SecretChange>,
        #[serde(default)]
        headers: BTreeMap<String, SecretChange>,
    },
    Reset {
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        environment: Vec<String>,
        #[serde(default)]
        headers: Vec<String>,
    },
    /// 绑定参数只提交原插件设置；与直接覆盖分开调用，影响共用该参数的服务。
    SetParameter {
        name: String,
        value: Option<String>,
        expected_settings_revision: String,
        expected_plugin_sha256: String,
    },
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConfigureInput {
    expected: ExpectedTarget,
    change: Change,
}

/// 授权阶段的冻结身份，不包含任何新值；审批前后与执行时均重新验证真实来源。
#[derive(Clone)]
pub(crate) struct McpManagementAuthorizationFacts {
    pub(crate) write: bool,
    expected: Option<ExpectedTarget>,
    parameter: Option<(String, String)>,
    management: McpManagement,
}

impl McpManagementAuthorizationFacts {
    pub(crate) async fn validate(&self) -> RuntimeResult<()> {
        if let Some(expected) = &self.expected {
            let target = expected
                .target
                .resolve()
                .map_err(|_| RuntimeError::McpConfigInvalid)?;
            let server = self
                .management
                .current_target(
                    &target,
                    &expected.expected_revision,
                    &expected.expected_definition_digest,
                )
                .await?;
            if let Some((revision, sha)) = &self.parameter {
                let managed = server
                    .managed
                    .as_ref()
                    .ok_or(RuntimeError::McpConfigInvalid)?;
                if managed.plugin_settings_revision.as_ref() != Some(revision)
                    || managed.plugin_sha256.as_ref() != Some(sha)
                {
                    return Err(RuntimeError::McpConfigConflict);
                }
            }
        }
        Ok(())
    }
}

impl McpManagement {
    async fn current_target(
        &self,
        target: &McpServerTarget,
        revision: &str,
        digest: &str,
    ) -> RuntimeResult<McpServerSnapshot> {
        let snapshot = self
            .get_mcp_configuration(protocol::GetMcpConfigurationRequest::default())
            .await?
            .snapshot;
        let server = snapshot
            .servers
            .into_iter()
            .find(|server| server.source == target.source && server.server_key == target.server_key)
            .ok_or(RuntimeError::McpServerUnavailable)?;
        let (current_revision, current_digest) =
            server
                .managed
                .as_ref()
                .map_or((snapshot.revision.as_str(), ""), |managed| {
                    (
                        managed.revision.as_str(),
                        managed.definition_digest.as_str(),
                    )
                });
        if current_revision != revision || current_digest != digest {
            return Err(RuntimeError::McpConfigConflict);
        }
        Ok(server)
    }
}

struct GetConfigurationTool {
    management: McpManagement,
    session: Arc<SessionController>,
}
struct ConfigureTool {
    management: McpManagement,
    session: Arc<SessionController>,
}
struct RefreshTool {
    management: McpManagement,
    session: Arc<SessionController>,
}

pub(in crate::runtime) fn tool_set(
    management: McpManagement,
    session: Arc<SessionController>,
) -> Result<ToolSetSnapshot, agent_tools::RegisterToolError> {
    let mut registry = ToolRegistry::new();
    registry.register(GetConfigurationTool {
        management: management.clone(),
        session: session.clone(),
    })?;
    registry.register(ConfigureTool {
        management: management.clone(),
        session: session.clone(),
    })?;
    registry.register(RefreshTool {
        management,
        session,
    })?;
    Ok(registry.snapshot())
}

fn active(session: &SessionController, context: &ToolContext) -> Result<(), ToolError> {
    session.ensure_active().map_err(safe_error)?;
    if context.cancellation.is_cancelled() {
        return Err(ToolError::execution("MCP management cancelled"));
    }
    Ok(())
}

fn safe_error(error: RuntimeError) -> ToolError {
    let code = error.to_protocol_info().code;
    ToolError::execution(format!(
        "MCP management failed: {code:?}; query current configuration before retrying"
    ))
}

impl Tool for GetConfigurationTool {
    type Input = GetInput;
    type ResolvedInput = Option<McpServerTarget>;
    type Output = Value;

    fn name(&self) -> ToolName {
        ToolName::new("get_mcp_configuration").expect("static tool name")
    }

    fn description(&self) -> String {
        "Read MCP configuration metadata and revisions for this user's Host, even with no connected servers. Existing secrets are never returned. Optionally filter by a real target (source={type:user}, {type:plugin,name:plugin-name}, or {type:builtin}). Query before editing; target may be disabled or missing required parameters.".into()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let target = input.target.as_ref().map(Target::resolve).transpose()?;
        Ok(ToolResolution::with_facts(
            target,
            McpManagementAuthorizationFacts {
                write: false,
                expected: None,
                parameter: None,
                management: self.management.clone(),
            },
            json!({"target":input.target}),
        ))
    }

    fn execute<'a>(
        &'a self,
        target: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            active(&self.session, &context)?;
            let mut snapshot = self
                .management
                .get_mcp_configuration(protocol::GetMcpConfigurationRequest::default())
                .await
                .map_err(safe_error)?
                .snapshot;
            if let Some(target) = target {
                snapshot.servers.retain(|server| {
                    server.source == target.source && server.server_key == target.server_key
                });
            }
            serde_json::to_value(snapshot)
                .map_err(|_| ToolError::execution("MCP metadata could not be encoded"))
        })
    }
}

impl Tool for ConfigureTool {
    type Input = ConfigureInput;
    type ResolvedInput = ConfigureInput;
    type Output = Value;

    fn name(&self) -> ToolName {
        ToolName::new("configure_mcp").expect("static tool name")
    }

    fn description(&self) -> String {
        "Build only: modify enabled/env/headers on one existing MCP target using revisions from get_mcp_configuration. No command, URL, package, permission or user selection. Patch uses keep/replace/remove; remove suppresses the default, reset restores managed defaults. For parameter_bindings use set_parameter in a separate call with plugin revisions; it changes the original shared plugin parameter. Direct saves require an explicit refresh_mcp to apply; newly enabled business tools enter the next Run because the current tool scope is frozen; original plugin parameter saves retain their existing apply behavior. Prefer user entry of secrets in settings. Values provided in this conversation may remain in its original messages/tool calls; this tool never reads back existing secrets.".into()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.expected.target.resolve()?;
        let parameter = match &input.change {
            Change::SetParameter {
                expected_settings_revision,
                expected_plugin_sha256,
                ..
            } => Some((
                expected_settings_revision.clone(),
                expected_plugin_sha256.clone(),
            )),
            _ => None,
        };
        let facts = McpManagementAuthorizationFacts {
            write: true,
            expected: Some(input.expected.clone()),
            parameter,
            management: self.management.clone(),
        };
        // 摘要只显示修改键；原始模型 ToolCall 沿现有历史保存，不能宣称已追溯移除秘密。
        let semantic = safe_arguments(
            &serde_json::to_value(&input)
                .map_err(|_| ToolError::invalid_input("invalid MCP change"))?,
        );
        Ok(ToolResolution::with_facts(input, facts, semantic))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            active(&self.session, &context)?;
            let expected = input.expected;
            let target = expected.target.resolve()?;
            let server = self
                .management
                .current_target(
                    &target,
                    &expected.expected_revision,
                    &expected.expected_definition_digest,
                )
                .await
                .map_err(safe_error)?;
            if let Change::SetParameter {
                name,
                value,
                expected_settings_revision,
                expected_plugin_sha256,
            } = input.change
            {
                let McpServerSource::Plugin(plugin_name) = &target.source else {
                    return Err(ToolError::invalid_input(
                        "set_parameter requires a plugin target",
                    ));
                };
                let managed = server
                    .managed
                    .ok_or_else(|| ToolError::invalid_input("managed metadata required"))?;
                if !managed
                    .parameter_bindings
                    .values()
                    .flatten()
                    .any(|binding| binding == &name)
                {
                    return Err(ToolError::invalid_input(
                        "parameter is not bound to this MCP service",
                    ));
                }
                self.management
                    .mutate_plugin_settings(protocol::MutatePluginSettingsRequest {
                        name: plugin_name.clone(),
                        expected_revision: expected_settings_revision,
                        expected_active_sha256: expected_plugin_sha256,
                        mutation: protocol::PluginSettingsMutation::SetParameter { name, value },
                    })
                    .await
                    .map_err(safe_error)?;
            } else {
                let mutation = mutation(
                    &server,
                    &target,
                    expected.expected_definition_digest,
                    input.change,
                )?;
                self.management
                    .mutate_mcp_configuration(protocol::MutateMcpConfigurationRequest {
                        expected_revision: expected.expected_revision,
                        mutation,
                    })
                    .await
                    .map_err(safe_error)?;
            }
            Ok(
                json!({"status":"saved", "target":expected.target, "message":"Configuration saved. Query current state; direct overrides need refresh_mcp. No MCP business tool was invoked."}),
            )
        })
    }
}

impl Tool for RefreshTool {
    type Input = ExpectedTarget;
    type ResolvedInput = ExpectedTarget;
    type Output = Value;

    fn name(&self) -> ToolName {
        ToolName::new("refresh_mcp").expect("static tool name")
    }

    fn description(&self) -> String {
        "Build only: apply saved configuration and refresh the connection/tool catalog for one explicit MCP target. Use current revision and definition_digest from get_mcp_configuration. This may launch its trusted process or connect to its server, but does not invoke business tools or perform a paid search.".into()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.target.resolve()?;
        let semantic = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("invalid MCP target"))?;
        Ok(ToolResolution::with_facts(
            input.clone(),
            McpManagementAuthorizationFacts {
                write: true,
                expected: Some(input),
                parameter: None,
                management: self.management.clone(),
            },
            semantic,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            active(&self.session, &context)?;
            let target = input.target.resolve()?;
            let result = self
                .management
                .refresh_mcp_server(
                    protocol::RefreshMcpServerRequest {
                        target,
                        expected_revision: input.expected_revision,
                        expected_definition_digest: input.expected_definition_digest,
                    },
                    context.cancellation,
                )
                .await
                .map_err(safe_error)?;
            serde_json::to_value(result)
                .map_err(|_| ToolError::execution("MCP refresh result could not be encoded"))
        })
    }
}

fn mutation(
    server: &McpServerSnapshot,
    target: &McpServerTarget,
    digest: String,
    change: Change,
) -> Result<protocol::McpConfigurationMutation, ToolError> {
    use protocol::{
        McpConfigurationMutation as Mutation, McpFieldChange as Field,
        McpServerTransportDraft as Transport,
    };
    match change {
        Change::Patch {
            enabled,
            environment,
            headers,
        } => {
            let environment: BTreeMap<_, _> = environment
                .into_iter()
                .map(|(key, change)| (key, change.into()))
                .collect();
            let headers: BTreeMap<_, _> = headers
                .into_iter()
                .map(|(key, change)| (key, change.into()))
                .collect();
            if target.source != McpServerSource::User {
                return Ok(Mutation::PatchManaged {
                    target: target.clone(),
                    expected_definition_digest: digest,
                    enabled,
                    environment,
                    headers,
                });
            }
            let transport = match server.transport {
                protocol::McpTransportKind::Stdio if headers.is_empty() => Transport::Stdio {
                    command: Field::Keep,
                    args: Field::Keep,
                    cwd: Field::Keep,
                    environment,
                },
                protocol::McpTransportKind::StreamableHttp if environment.is_empty() => {
                    Transport::StreamableHttp {
                        url: Field::Keep,
                        headers,
                    }
                }
                _ => {
                    return Err(ToolError::invalid_input(
                        "env requires stdio; headers require HTTP",
                    ));
                }
            };
            Ok(Mutation::Upsert {
                server: protocol::McpServerDraft {
                    server_key: server.server_key.clone(),
                    display_name: server.display_name.clone(),
                    description: server.description.clone(),
                    enabled: enabled.unwrap_or(server.enabled),
                    transport,
                    startup_timeout_ms: server.startup_timeout_ms,
                    tool_timeout_ms: server.tool_timeout_ms,
                },
            })
        }
        Change::Reset {
            enabled,
            environment,
            headers,
        } if target.source != McpServerSource::User => Ok(Mutation::ResetManaged {
            target: target.clone(),
            expected_definition_digest: digest,
            enabled,
            environment,
            headers,
        }),
        _ => Err(ToolError::invalid_input("reset requires a managed target")),
    }
}

/// 供审批、活动事件及工具详情共用；替换所有字段值，不依赖变量名称是否像凭据。
pub(in crate::runtime) fn safe_arguments(arguments: &Value) -> Value {
    let mut safe = arguments.clone();
    if let Some(change) = safe.get_mut("change").and_then(Value::as_object_mut) {
        for field in ["environment", "headers"] {
            if let Some(values) = change.get_mut(field).and_then(Value::as_object_mut) {
                for value in values.values_mut() {
                    let mode = value
                        .get("mode")
                        .and_then(Value::as_str)
                        .unwrap_or("invalid")
                        .to_owned();
                    *value = if mode == "keep" || mode == "remove" {
                        json!({"mode":mode})
                    } else {
                        json!({"mode":mode,"value":"***"})
                    };
                }
            }
        }
        if change.contains_key("value") {
            change.insert("value".into(), Value::String("***".into()));
        }
    }
    safe
}
