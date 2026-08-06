//! Core Demo 私有的工作模式、名单规则与 Authorizer 装配。

use std::sync::Arc;

use agent_core::{
    ComposedToolAuthorizer, FileToolPolicyAdapter, GeneralToolPolicyAdapter, PolicyEvaluation,
    ShellToolPolicyAdapter, ToolAuthorization, ToolAuthorizer, ToolPolicy, TypedToolPolicy,
};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileOperation, GeneralAuthorizationFacts,
    ResolvedToolBatch, ResolvedToolInvocation, ShellAuthorizationFacts,
};

use crate::{
    audit::{AuditDecision, DemoAudit},
    wire::ApprovalMode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuleEffect {
    Allow,
    Deny,
}

impl RuleEffect {
    fn evaluation(self, reason: &str) -> PolicyEvaluation {
        PolicyEvaluation::Decide(match self {
            Self::Allow => ToolAuthorization::Allow,
            Self::Deny => ToolAuthorization::Deny {
                reason: reason.to_owned(),
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathScope {
    Recursive(AbsolutePath),
}

impl PathScope {
    fn matches(&self, path: &AbsolutePath) -> bool {
        match self {
            Self::Recursive(root) => path.as_path().starts_with(root.as_path()),
        }
    }
}

#[derive(Clone, Debug)]
struct FileRule {
    effect: RuleEffect,
    operations: Vec<FileOperation>,
    scope: PathScope,
    description: &'static str,
}

#[derive(Clone, Debug, Default)]
struct FileRuleSet {
    rules: Vec<FileRule>,
}

impl TypedToolPolicy<FileAuthorizationFacts> for FileRuleSet {
    fn evaluate(
        &self,
        facts: &FileAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        self.rules
            .iter()
            .find(|rule| rule.effect == RuleEffect::Allow && file_rule_matches(rule, facts))
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|rule| rule.effect == RuleEffect::Deny && file_rule_matches(rule, facts))
            })
            .map_or(PolicyEvaluation::Continue, |rule| {
                rule.effect.evaluation(rule.description)
            })
    }
}

fn file_rule_matches(rule: &FileRule, facts: &FileAuthorizationFacts) -> bool {
    rule.operations.contains(&facts.operation) && rule.scope.matches(&facts.path)
}

struct PlanFilePolicy {
    session_workdir: AbsolutePath,
    temporary_workspace: AbsolutePath,
}

impl TypedToolPolicy<FileAuthorizationFacts> for PlanFilePolicy {
    fn evaluate(
        &self,
        facts: &FileAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        if facts
            .path
            .as_path()
            .starts_with(self.temporary_workspace.as_path())
        {
            return RuleEffect::Allow.evaluation("plan temporary workspace capability");
        }
        let read_only = matches!(
            facts.operation,
            FileOperation::Read | FileOperation::List | FileOperation::Find | FileOperation::Search
        );
        if read_only
            && facts
                .path
                .as_path()
                .starts_with(self.session_workdir.as_path())
        {
            return RuleEffect::Allow.evaluation("plan session workdir read capability");
        }
        RuleEffect::Deny.evaluation("plan mode does not permit this file operation")
    }
}

struct DenyAllShell;
impl TypedToolPolicy<ShellAuthorizationFacts> for DenyAllShell {
    fn evaluate(
        &self,
        _facts: &ShellAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        RuleEffect::Deny.evaluation("plan mode does not permit shell execution")
    }
}

struct PlanGeneralPolicy;
impl TypedToolPolicy<GeneralAuthorizationFacts> for PlanGeneralPolicy {
    fn evaluate(
        &self,
        facts: &GeneralAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        if is_read_only_memory_tool(facts.tool_name.as_str()) {
            RuleEffect::Allow.evaluation("plan memory read capability")
        } else {
            RuleEffect::Deny.evaluation("plan mode does not permit this general tool")
        }
    }
}

#[derive(Default)]
struct ContinueShell;
impl TypedToolPolicy<ShellAuthorizationFacts> for ContinueShell {
    fn evaluate(
        &self,
        _facts: &ShellAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        PolicyEvaluation::Continue
    }
}

#[derive(Default)]
struct BuildGeneralPolicy;
impl TypedToolPolicy<GeneralAuthorizationFacts> for BuildGeneralPolicy {
    fn evaluate(
        &self,
        facts: &GeneralAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        if is_read_only_memory_tool(facts.tool_name.as_str()) {
            RuleEffect::Allow.evaluation("trusted memory read")
        } else {
            PolicyEvaluation::Continue
        }
    }
}

fn is_read_only_memory_tool(tool_name: &str) -> bool {
    matches!(tool_name, "list_pinned_memories" | "recall_memory")
}

struct AuditedPolicy {
    name: &'static str,
    run_id: String,
    inner: Arc<dyn ToolPolicy>,
    audit: DemoAudit,
}

impl ToolPolicy for AuditedPolicy {
    fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        let evaluation = self.inner.evaluate(invocation, batch);
        if let PolicyEvaluation::Decide(decision) = &evaluation {
            let (audit_decision, rule) = match decision {
                ToolAuthorization::Allow => (AuditDecision::Allow, "matched allow rule"),
                ToolAuthorization::Deny { reason } => (AuditDecision::Deny, reason.as_str()),
            };
            self.audit
                .record_policy(&self.run_id, invocation, self.name, rule, audit_decision);
        }
        evaluation
    }
}

struct AuditedAllowAll {
    run_id: String,
    audit: DemoAudit,
}

struct AuditedDenyAll {
    run_id: String,
    audit: DemoAudit,
}

impl ToolAuthorizer for AuditedDenyAll {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> agent_core::AuthorizationFuture<'a> {
        self.audit.record_policy(
            &self.run_id,
            invocation,
            "plan_fallback",
            "unclassified plan invocation",
            AuditDecision::Deny,
        );
        Box::pin(std::future::ready(ToolAuthorization::Deny {
            reason: "plan mode does not permit unclassified tool authorization facts".to_owned(),
        }))
    }
}
impl ToolAuthorizer for AuditedAllowAll {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> agent_core::AuthorizationFuture<'a> {
        self.audit.record_policy(
            &self.run_id,
            invocation,
            "auto_allow_all",
            "unmatched build invocation",
            AuditDecision::Allow,
        );
        Box::pin(std::future::ready(ToolAuthorization::Allow))
    }
}

pub(crate) fn plan_authorizer(
    run_id: &str,
    session_workdir: AbsolutePath,
    temporary_workspace: AbsolutePath,
    audit: DemoAudit,
) -> Arc<dyn ToolAuthorizer> {
    let policies: Vec<Arc<dyn ToolPolicy>> = vec![
        Arc::new(FileToolPolicyAdapter::new(PlanFilePolicy {
            session_workdir,
            temporary_workspace,
        })),
        Arc::new(ShellToolPolicyAdapter::new(DenyAllShell)),
        Arc::new(GeneralToolPolicyAdapter::new(PlanGeneralPolicy)),
    ];
    Arc::new(ComposedToolAuthorizer::new(
        audited("plan_capability", run_id, audit.clone(), policies),
        Arc::new(AuditedDenyAll {
            run_id: run_id.to_owned(),
            audit,
        }),
    ))
}

pub(crate) fn build_authorizer(
    run_id: &str,
    session_workdir: AbsolutePath,
    temporary_workspace: AbsolutePath,
    denied_workspace: AbsolutePath,
    approval_mode: ApprovalMode,
    approval_authorizer: Arc<dyn ToolAuthorizer>,
    audit: DemoAudit,
) -> Arc<dyn ToolAuthorizer> {
    let read_only = vec![
        FileOperation::Read,
        FileOperation::List,
        FileOperation::Find,
        FileOperation::Search,
    ];
    let all_file = vec![
        FileOperation::Read,
        FileOperation::List,
        FileOperation::Find,
        FileOperation::Search,
        FileOperation::Write,
        FileOperation::Edit,
        FileOperation::Delete,
    ];
    let policies: Vec<Arc<dyn ToolPolicy>> = vec![
        Arc::new(FileToolPolicyAdapter::new(FileRuleSet {
            rules: vec![
                FileRule {
                    effect: RuleEffect::Allow,
                    operations: all_file.clone(),
                    scope: PathScope::Recursive(temporary_workspace),
                    description: "trusted temporary workspace",
                },
                FileRule {
                    effect: RuleEffect::Allow,
                    operations: read_only,
                    scope: PathScope::Recursive(session_workdir),
                    description: "trusted session workdir read",
                },
                FileRule {
                    effect: RuleEffect::Deny,
                    operations: all_file,
                    scope: PathScope::Recursive(denied_workspace),
                    description: "explicit demo deny scope",
                },
            ],
        })),
        Arc::new(ShellToolPolicyAdapter::new(ContinueShell)),
        Arc::new(GeneralToolPolicyAdapter::new(BuildGeneralPolicy)),
    ];
    let final_authorizer: Arc<dyn ToolAuthorizer> = match approval_mode {
        ApprovalMode::Ask => approval_authorizer,
        ApprovalMode::Auto => Arc::new(AuditedAllowAll {
            run_id: run_id.to_owned(),
            audit: audit.clone(),
        }),
    };
    Arc::new(ComposedToolAuthorizer::new(
        audited("build_rules", run_id, audit, policies),
        final_authorizer,
    ))
}

fn audited(
    name: &'static str,
    run_id: &str,
    audit: DemoAudit,
    policies: Vec<Arc<dyn ToolPolicy>>,
) -> Vec<Arc<dyn ToolPolicy>> {
    policies
        .into_iter()
        .map(|inner| {
            Arc::new(AuditedPolicy {
                name,
                run_id: run_id.to_owned(),
                inner,
                audit: audit.clone(),
            }) as Arc<dyn ToolPolicy>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use agent_tools::{
        Dispatcher, ResolvedBatchItemRef, Tool, ToolContext, ToolError, ToolExecuteFuture,
        ToolRegistry, ToolResolution,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Deserialize, JsonSchema, Serialize)]
    struct UnclassifiedInput {}

    struct UnclassifiedFacts;

    struct UnclassifiedTool;

    impl Tool for UnclassifiedTool {
        type Input = UnclassifiedInput;
        type ResolvedInput = UnclassifiedInput;
        type Output = UnclassifiedInput;

        fn name(&self) -> ToolName {
            ToolName::new("unclassified_tool").expect("valid tool name")
        }

        fn description(&self) -> String {
            "tool with authorization facts unknown to the demo policies".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::with_facts(
                input,
                UnclassifiedFacts,
                serde_json::json!({}),
            ))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    fn unclassified_batch() -> ResolvedToolBatch {
        let mut registry = ToolRegistry::new();
        registry
            .register(UnclassifiedTool)
            .expect("register unclassified tool");
        Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[ToolCall {
                id: ToolCallId::new("call_unclassified").expect("valid call id"),
                name: ToolName::new("unclassified_tool").expect("valid tool name"),
                arguments: serde_json::json!({}),
            }],
        )
    }

    #[test]
    fn allow_rule_has_priority_over_overlapping_deny() {
        let root = tempfile::tempdir().expect("temp root");
        let denied = AbsolutePath::new(root.path().join("denied")).expect("absolute denied");
        let facts = FileAuthorizationFacts {
            operation: FileOperation::Read,
            path: AbsolutePath::new(root.path().join("denied/file.txt")).expect("absolute file"),
        };
        let rules = FileRuleSet {
            rules: vec![
                FileRule {
                    effect: RuleEffect::Deny,
                    operations: vec![FileOperation::Read],
                    scope: PathScope::Recursive(denied.clone()),
                    description: "deny",
                },
                FileRule {
                    effect: RuleEffect::Allow,
                    operations: vec![FileOperation::Read],
                    scope: PathScope::Recursive(denied),
                    description: "allow",
                },
            ],
        };
        // Typed policies ignore invocation/batch for these facts, so verify the selection helper directly.
        let matched = rules
            .rules
            .iter()
            .find(|rule| rule.effect == RuleEffect::Allow && file_rule_matches(rule, &facts));
        assert!(matched.is_some());
    }

    #[tokio::test]
    async fn plan_denies_authorization_facts_not_handled_by_any_policy() {
        let root = tempfile::tempdir().expect("temp root");
        let workdir = AbsolutePath::new(root.path()).expect("absolute workdir");
        let temporary_workspace =
            AbsolutePath::new(root.path().join("temporary")).expect("absolute temporary path");
        let audit = DemoAudit::default();
        let authorizer = plan_authorizer(
            "run-unclassified",
            workdir,
            temporary_workspace,
            audit.clone(),
        );
        let batch = unclassified_batch();
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("custom tool resolves");
        };

        assert_eq!(
            authorizer.authorize(invocation, &batch).await,
            ToolAuthorization::Deny {
                reason: "plan mode does not permit unclassified tool authorization facts"
                    .to_owned(),
            }
        );
        let entries = audit.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].policy, "plan_fallback");
        assert_eq!(entries[0].decision, Some(AuditDecision::Deny));
    }
}
