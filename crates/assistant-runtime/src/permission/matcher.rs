//! 权限 JSON matcher 到 resolved typed facts 的确定性匹配。

use std::path::Path;

use agent_tools::{
    FileAuthorizationFacts, FileBatchAuthorizationFacts, FileOperation, GeneralAuthorizationFacts,
    ResolvedToolInvocation, ShellAuthorizationFacts, ShellProcessMode,
};
use assistant_protocol::AgentVariant;

use super::{
    CommandMatch, McpPermissionServerMatch, McpPermissionToolMatch, PathMatch,
    PermissionFileOperation, PermissionMatcher, PermissionProcessMode, PermissionRule,
};
use crate::{delegation::DelegationAuthorizationFacts, mcp::McpAuthorizationFacts};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvocationFactKind {
    General,
    File,
    Shell,
    Mcp,
    Unknown,
}

pub(crate) fn fact_kind(invocation: &ResolvedToolInvocation) -> InvocationFactKind {
    if invocation.facts::<FileAuthorizationFacts>().is_some()
        || invocation.facts::<FileBatchAuthorizationFacts>().is_some()
    {
        InvocationFactKind::File
    } else if invocation.facts::<ShellAuthorizationFacts>().is_some() {
        InvocationFactKind::Shell
    } else if invocation.facts::<McpAuthorizationFacts>().is_some() {
        InvocationFactKind::Mcp
    } else if invocation.facts::<GeneralAuthorizationFacts>().is_some()
        || invocation.facts::<DelegationAuthorizationFacts>().is_some()
    {
        InvocationFactKind::General
    } else {
        InvocationFactKind::Unknown
    }
}

pub(crate) fn matches_rule(
    rule: &PermissionRule,
    variant: AgentVariant,
    invocation: &ResolvedToolInvocation,
) -> bool {
    rule.variants.contains(&variant)
        && match &rule.matcher {
            PermissionMatcher::General(matcher) => {
                invocation
                    .facts::<GeneralAuthorizationFacts>()
                    .is_some_and(|facts| facts.tool_name.as_str() == matcher.tool_name)
                    || invocation
                        .facts::<DelegationAuthorizationFacts>()
                        .is_some_and(|_| invocation.tool_name().as_str() == matcher.tool_name)
            }
            PermissionMatcher::File(matcher) => {
                invocation
                    .facts::<FileAuthorizationFacts>()
                    .is_some_and(|facts| {
                        file_matcher_matches(matcher, facts.operation, &facts.path)
                    })
                    || invocation
                        .facts::<FileBatchAuthorizationFacts>()
                        .is_some_and(|facts| {
                            let matches = |path: &agent_tools::AbsolutePath| {
                                file_matcher_matches(matcher, facts.operation, path)
                            };
                            match rule.effect {
                                super::PermissionEffect::Allow => facts.paths.iter().all(matches),
                                super::PermissionEffect::Ask | super::PermissionEffect::Deny => {
                                    facts.paths.iter().any(matches)
                                }
                            }
                        })
            }
            PermissionMatcher::Shell(matcher) => invocation
                .facts::<ShellAuthorizationFacts>()
                .is_some_and(|facts| {
                    command_matches(&facts.command, &matcher.command, matcher.command_match)
                        && facts.workdir.as_path() == Path::new(&matcher.working_directory)
                        && process_mode(facts.process_mode) == matcher.process_mode
                }),
            PermissionMatcher::Mcp(matcher) => invocation
                .facts::<McpAuthorizationFacts>()
                .is_some_and(|facts| {
                    mcp_matcher_matches(
                        matcher,
                        &facts.invocation.server_key,
                        &facts.invocation.tool_name,
                    )
                }),
        }
}

pub(crate) fn mcp_matcher_matches(
    matcher: &super::McpPermissionMatcher,
    server_key: &assistant_protocol::McpServerKey,
    tool_name: &str,
) -> bool {
    let server_matches = match &matcher.server {
        McpPermissionServerMatch::Any => true,
        McpPermissionServerMatch::Exact { value } => value == server_key,
    };
    let tool_matches = match &matcher.tool {
        McpPermissionToolMatch::Any => true,
        McpPermissionToolMatch::Exact { value } => value == tool_name,
    };
    server_matches && tool_matches
}

pub(crate) fn file_matcher_matches(
    matcher: &super::FilePermissionMatcher,
    operation: FileOperation,
    path: &agent_tools::AbsolutePath,
) -> bool {
    file_operation(operation) == matcher.operation
        && path_matches(path.as_path(), Path::new(&matcher.path), matcher.path_match)
}

/// 只有能完整表达一次已解析调用事实的 Allow 规则才可以用于审批队列自动重核。
/// General、递归路径和命令前缀都可能覆盖用户尚未查看的其他调用，不能作为 drain 依据。
pub(crate) fn is_exact_allow_rule(rule: &PermissionRule) -> bool {
    rule.effect == super::PermissionEffect::Allow
        && match &rule.matcher {
            PermissionMatcher::General(_) => false,
            PermissionMatcher::File(matcher) => matcher.path_match == PathMatch::Exact,
            PermissionMatcher::Shell(matcher) => matcher.command_match == CommandMatch::Exact,
            PermissionMatcher::Mcp(matcher) => {
                matches!(&matcher.server, McpPermissionServerMatch::Exact { .. })
                    && matches!(&matcher.tool, McpPermissionToolMatch::Exact { .. })
            }
        }
}

fn file_operation(operation: FileOperation) -> PermissionFileOperation {
    match operation {
        FileOperation::Read => PermissionFileOperation::Read,
        FileOperation::List => PermissionFileOperation::List,
        FileOperation::Find => PermissionFileOperation::Find,
        FileOperation::Search => PermissionFileOperation::Search,
        FileOperation::Write => PermissionFileOperation::Write,
        FileOperation::Edit => PermissionFileOperation::Edit,
        FileOperation::Delete => PermissionFileOperation::Delete,
    }
}

fn path_matches(actual: &Path, configured: &Path, mode: PathMatch) -> bool {
    match mode {
        PathMatch::Exact => actual == configured,
        PathMatch::Recursive => actual == configured || actual.starts_with(configured),
    }
}

fn command_matches(actual: &str, configured: &str, mode: CommandMatch) -> bool {
    match mode {
        CommandMatch::Exact => actual == configured,
        CommandMatch::Prefix => actual.starts_with(configured),
    }
}

fn process_mode(mode: ShellProcessMode) -> PermissionProcessMode {
    match mode {
        ShellProcessMode::Managed => PermissionProcessMode::Managed,
        ShellProcessMode::Detached => PermissionProcessMode::Detached,
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, sync::Arc, time::Duration};

    use agent_testkit::{OrderLog, ScriptedTool};
    use agent_tools::{
        AbsolutePath, Dispatcher, ImageInspectionFuture, ImageInspector, InspectImagesRequest,
        InspectImagesTool, ResolvedBatchItemRef, SessionPathResolver, ShellExecTool,
        ShellExecToolConfig, ShellFuture, ShellOutputSink, ShellRequest, ShellTool, ShellToolError,
        Tool, ToolContext, ToolError, ToolExecuteFuture, ToolRegistry, ToolResolution,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::permission::{
        GeneralPermissionMatcher, PermissionEffect, PermissionMatcher, PermissionRule,
        ShellPermissionMatcher,
    };

    struct NeverShell;

    struct NeverInspector;

    #[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
    struct McpInput {
        server: String,
        tool: String,
    }

    struct McpFactsTool;

    impl Tool for McpFactsTool {
        type Input = McpInput;
        type ResolvedInput = McpInput;
        type Output = serde_json::Value;

        fn name(&self) -> ToolName {
            ToolName::new("call_mcp_tool").expect("name")
        }

        fn description(&self) -> String {
            "fixture".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            let facts = McpAuthorizationFacts {
                invocation: crate::mcp::ResolvedMcpInvocation::unavailable_for_test(
                    assistant_protocol::McpServerKey::new(&input.server).expect("server"),
                    input.tool.clone(),
                ),
            };
            Ok(ToolResolution::with_facts(
                input.clone(),
                facts,
                json!({"server": input.server, "tool": input.tool}),
            ))
        }

        fn execute<'a>(
            &'a self,
            _input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::pending())
        }
    }

    impl ImageInspector for NeverInspector {
        fn inspect<'a>(
            &'a self,
            _request: InspectImagesRequest,
            _cancellation: &'a CancellationToken,
        ) -> ImageInspectionFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    impl ShellTool for NeverShell {
        fn exec<'a>(
            &'a self,
            _request: ShellRequest,
            _sink: ShellOutputSink,
            _cancellation: CancellationToken,
        ) -> ShellFuture<'a> {
            Box::pin(std::future::ready(Err(ShellToolError::InvalidInput {
                message: "not executed".to_owned(),
            })))
        }
    }

    #[test]
    fn general_matcher_uses_the_resolved_tool_name_and_variant() {
        let mut registry = ToolRegistry::new();
        registry
            .register(ScriptedTool::succeed(
                "inspect",
                json!({"ok": true}),
                OrderLog::default(),
            ))
            .expect("register tool");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[call("inspect", json!({"raw": true}))],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("item") else {
            panic!("general call resolves");
        };
        let rule = PermissionRule {
            id: "general".to_owned(),
            effect: PermissionEffect::Allow,
            variants: vec![AgentVariant::Plan],
            matcher: PermissionMatcher::General(GeneralPermissionMatcher {
                tool_name: "inspect".to_owned(),
            }),
        };
        assert!(matches_rule(&rule, AgentVariant::Plan, invocation));
        assert!(!matches_rule(&rule, AgentVariant::Build, invocation));
    }

    #[test]
    fn shell_matcher_uses_full_command_workdir_and_process_mode() {
        let workdir = std::env::temp_dir();
        let mut registry = ToolRegistry::new();
        registry
            .register(ShellExecTool::new(
                Arc::new(NeverShell),
                SessionPathResolver::new(AbsolutePath::new(&workdir).expect("temp path")),
                ShellExecToolConfig::new(
                    Duration::from_secs(5),
                    Duration::from_secs(10),
                    NonZeroU64::new(1024).expect("nonzero"),
                )
                .expect("shell config"),
            ))
            .expect("register shell");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[call(
                "shell",
                json!({"command": "git status --short", "workdir": workdir}),
            )],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("item") else {
            panic!("shell resolves");
        };
        let rule = PermissionRule {
            id: "shell".to_owned(),
            effect: PermissionEffect::Ask,
            variants: vec![AgentVariant::Build],
            matcher: PermissionMatcher::Shell(ShellPermissionMatcher {
                command: "git status".to_owned(),
                command_match: CommandMatch::Prefix,
                working_directory: workdir.to_string_lossy().into_owned(),
                process_mode: PermissionProcessMode::Managed,
            }),
        };
        assert!(matches_rule(&rule, AgentVariant::Build, invocation));
        let mut exact_rule = rule.clone();
        let PermissionMatcher::Shell(matcher) = &mut exact_rule.matcher else {
            unreachable!();
        };
        matcher.command_match = CommandMatch::Exact;
        // Exact 与 prefix 的语义不同，完整命令不能被缩写规则命中。
        assert!(!matches_rule(&exact_rule, AgentVariant::Build, invocation));
    }

    #[test]
    fn batch_file_rules_allow_all_paths_but_deny_or_ask_any_path() {
        let mut registry = ToolRegistry::new();
        registry
            .register(InspectImagesTool::new(
                Arc::new(NeverInspector),
                SessionPathResolver::new(AbsolutePath::new("/workspace").expect("workspace")),
            ))
            .expect("register inspect images");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[call(
                "inspect_images",
                json!({
                    "image_paths": ["a.png", "/session/private/b.png"],
                    "goal": "compare"
                }),
            )],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("item") else {
            panic!("inspect images resolves");
        };
        let rule = |effect, path: &str| PermissionRule {
            id: format!("{effect:?}-{path}"),
            effect,
            variants: vec![AgentVariant::Build],
            matcher: PermissionMatcher::File(crate::permission::FilePermissionMatcher {
                operation: PermissionFileOperation::Read,
                path: path.to_owned(),
                path_match: PathMatch::Recursive,
            }),
        };

        assert!(!matches_rule(
            &rule(PermissionEffect::Allow, "/workspace"),
            AgentVariant::Build,
            invocation
        ));
        assert!(matches_rule(
            &rule(PermissionEffect::Allow, "/"),
            AgentVariant::Build,
            invocation
        ));
        assert!(matches_rule(
            &rule(PermissionEffect::Deny, "/session/private"),
            AgentVariant::Build,
            invocation
        ));
        assert!(matches_rule(
            &rule(PermissionEffect::Ask, "/workspace"),
            AgentVariant::Build,
            invocation
        ));
    }

    #[test]
    fn mcp_matcher_requires_the_actual_server_and_raw_tool_identity() {
        let mut registry = ToolRegistry::new();
        registry
            .register(McpFactsTool)
            .expect("register MCP fixture");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[call(
                "call_mcp_tool",
                json!({"server": "github", "tool": "create_issue"}),
            )],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("item") else {
            panic!("MCP gateway resolves");
        };
        assert_eq!(fact_kind(invocation), InvocationFactKind::Mcp);

        let rule = |server, tool| PermissionRule {
            id: "mcp".to_owned(),
            effect: PermissionEffect::Allow,
            variants: vec![AgentVariant::Plan],
            matcher: PermissionMatcher::Mcp(crate::permission::McpPermissionMatcher {
                server,
                tool,
            }),
        };
        let exact = rule(
            McpPermissionServerMatch::Exact {
                value: assistant_protocol::McpServerKey::new("github").expect("server"),
            },
            McpPermissionToolMatch::Exact {
                value: "create_issue".to_owned(),
            },
        );
        assert!(matches_rule(&exact, AgentVariant::Plan, invocation));
        assert!(is_exact_allow_rule(&exact));
        assert!(!matches_rule(
            &rule(
                McpPermissionServerMatch::Exact {
                    value: assistant_protocol::McpServerKey::new("gitlab").expect("server"),
                },
                McpPermissionToolMatch::Any,
            ),
            AgentVariant::Plan,
            invocation,
        ));
        let all = rule(McpPermissionServerMatch::Any, McpPermissionToolMatch::Any);
        assert!(matches_rule(&all, AgentVariant::Plan, invocation));
        assert!(!is_exact_allow_rule(&all));
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(format!("call-{name}")).expect("call id"),
            name: ToolName::new(name).expect("tool name"),
            arguments,
        }
    }
}
