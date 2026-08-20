//! 权限 JSON matcher 到 resolved typed facts 的确定性匹配。

use std::path::Path;

use agent_tools::{
    FileAuthorizationFacts, FileBatchAuthorizationFacts, FileOperation, GeneralAuthorizationFacts,
    ResolvedToolInvocation, ShellAuthorizationFacts, ShellProcessMode,
};
use assistant_protocol::AgentVariant;

use super::{
    CommandMatch, PathMatch, PermissionFileOperation, PermissionMatcher, PermissionProcessMode,
    PermissionRule,
};
use crate::delegation::DelegationAuthorizationFacts;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvocationFactKind {
    General,
    File,
    Shell,
    Unknown,
}

pub(crate) fn fact_kind(invocation: &ResolvedToolInvocation) -> InvocationFactKind {
    if invocation.facts::<FileAuthorizationFacts>().is_some()
        || invocation.facts::<FileBatchAuthorizationFacts>().is_some()
    {
        InvocationFactKind::File
    } else if invocation.facts::<ShellAuthorizationFacts>().is_some() {
        InvocationFactKind::Shell
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
        }
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
        ToolRegistry,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::permission::{
        GeneralPermissionMatcher, PermissionEffect, PermissionMatcher, PermissionRule,
        ShellPermissionMatcher,
    };

    struct NeverShell;

    struct NeverInspector;

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

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(format!("call-{name}")).expect("call id"),
            name: ToolName::new(name).expect("tool name"),
            arguments,
        }
    }
}
