//! Demo 私有工作模式、名单规则与 Core 策略装配。

use std::sync::Arc;

use agent_core::{
    AllowAllAuthorizer, ComposedToolAuthorizer, FileToolPolicyAdapter, GeneralToolPolicyAdapter,
    PolicyEvaluation, ShellToolPolicyAdapter, ToolAuthorization, ToolAuthorizer, ToolPolicy,
    TypedToolPolicy,
};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileOperation, GeneralAuthorizationFacts,
    ResolvedToolBatch, ResolvedToolInvocation, ShellAuthorizationFacts,
};
use serde::{Deserialize, Serialize};

use crate::audit::{AuditDecision, DemoAudit};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
/// Run 的能力边界维度；Plan 比 Build 更严格，且不会被审批模式越过。
pub(crate) enum ExecutionMode {
    /// 仅允许项目只读文件能力和临时工作区文件能力。
    Plan,
    /// 使用信任规则，未命中项再交给审批维度处理。
    Build,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
/// 未命中 Build 信任规则时采用的处理方式。
pub(crate) enum ApprovalMode {
    /// 创建一次性审批并等待用户选择。
    Ask,
    /// 直接使用最终 AllowAll；仍不能覆盖前置策略的明确 Deny。
    Auto,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 名单规则命中后的明确效果。
pub(crate) enum RuleEffect {
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

/// 逻辑路径作用域；Recursive 使用词法 `Path::starts_with`，不解析符号链接。
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "the first demo rule configuration only uses recursive scopes"
)]
pub(crate) enum PathScope {
    Exact(AbsolutePath),
    Recursive(AbsolutePath),
}

impl PathScope {
    fn matches(&self, path: &AbsolutePath) -> bool {
        match self {
            Self::Exact(expected) => expected == path,
            Self::Recursive(root) => path.as_path().starts_with(root.as_path()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "shell trusted rules are supported but empty by default"
)]
pub(crate) enum CommandPattern {
    Exact(String),
    Prefix(String),
}

impl CommandPattern {
    fn matches(&self, command: &str) -> bool {
        match self {
            Self::Exact(expected) => command == expected,
            Self::Prefix(prefix) => command.starts_with(prefix),
        }
    }
}

#[derive(Clone, Debug)]
/// 通用工具规则；按完整工具名匹配。
pub(crate) struct GeneralRule {
    pub effect: RuleEffect,
    pub tool_name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
/// 通用工具的有序规则集合；集合内部统一执行 Allow 优先 Deny。
pub(crate) struct GeneralRuleSet {
    pub rules: Vec<GeneralRule>,
}

impl TypedToolPolicy<GeneralAuthorizationFacts> for GeneralRuleSet {
    fn evaluate(
        &self,
        facts: &GeneralAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        evaluate_allow_first(&self.rules, |rule| {
            rule.tool_name == facts.tool_name.as_str()
        })
        .map_or(PolicyEvaluation::Continue, |rule| {
            rule.effect.evaluation(&rule.description)
        })
    }
}

#[derive(Clone, Debug)]
/// 文件规则；操作类型和逻辑绝对路径必须同时命中。
pub(crate) struct FileRule {
    pub effect: RuleEffect,
    pub operations: Vec<FileOperation>,
    pub scope: PathScope,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
/// 文件工具的有序规则集合。
pub(crate) struct FileRuleSet {
    pub rules: Vec<FileRule>,
}

impl TypedToolPolicy<FileAuthorizationFacts> for FileRuleSet {
    fn evaluate(
        &self,
        facts: &FileAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        evaluate_allow_first(&self.rules, |rule| {
            rule.operations.contains(&facts.operation) && rule.scope.matches(&facts.path)
        })
        .map_or(PolicyEvaluation::Continue, |rule| {
            rule.effect.evaluation(&rule.description)
        })
    }
}

#[derive(Clone, Debug)]
/// Shell 规则；匹配完整 command，并可额外约束逻辑工作目录。
pub(crate) struct ShellRule {
    pub effect: RuleEffect,
    pub command: CommandPattern,
    pub workdir: Option<PathScope>,
    pub description: String,
}

#[derive(Clone, Debug, Default)]
/// Shell 工具的有序规则集合。
pub(crate) struct ShellRuleSet {
    pub rules: Vec<ShellRule>,
}

impl TypedToolPolicy<ShellAuthorizationFacts> for ShellRuleSet {
    fn evaluate(
        &self,
        facts: &ShellAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        evaluate_allow_first(&self.rules, |rule| {
            rule.command.matches(&facts.command)
                && rule
                    .workdir
                    .as_ref()
                    .is_none_or(|scope| scope.matches(&facts.workdir))
        })
        .map_or(PolicyEvaluation::Continue, |rule| {
            rule.effect.evaluation(&rule.description)
        })
    }
}

fn evaluate_allow_first<T>(rules: &[T], matches: impl Fn(&T) -> bool) -> Option<&T>
where
    T: HasEffect,
{
    rules
        .iter()
        .find(|rule| rule.effect() == RuleEffect::Allow && matches(rule))
        .or_else(|| {
            rules
                .iter()
                .find(|rule| rule.effect() == RuleEffect::Deny && matches(rule))
        })
}

trait HasEffect {
    fn effect(&self) -> RuleEffect;
}

impl HasEffect for GeneralRule {
    fn effect(&self) -> RuleEffect {
        self.effect
    }
}

impl HasEffect for FileRule {
    fn effect(&self) -> RuleEffect {
        self.effect
    }
}

impl HasEffect for ShellRule {
    fn effect(&self) -> RuleEffect {
        self.effect
    }
}

/// Plan 对文件调用的完整能力上限；所有文件事实都会得到明确决策。
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
        let is_read_only = matches!(
            facts.operation,
            FileOperation::Read | FileOperation::List | FileOperation::Find | FileOperation::Search
        );
        if is_read_only
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

struct DenyAllGeneral;

impl TypedToolPolicy<GeneralAuthorizationFacts> for DenyAllGeneral {
    fn evaluate(
        &self,
        _facts: &GeneralAuthorizationFacts,
        _invocation: &ResolvedToolInvocation,
        _batch: &ResolvedToolBatch,
    ) -> PolicyEvaluation {
        RuleEffect::Deny.evaluation("plan mode does not permit unclassified tools")
    }
}

/// 为策略增加同步审计，不改变 Core 的第一明确决策语义。
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
            let audit_decision = match decision {
                ToolAuthorization::Allow => AuditDecision::Allow,
                ToolAuthorization::Deny { .. } => AuditDecision::Deny,
            };
            let rule = match decision {
                ToolAuthorization::Allow => "matched allow rule",
                ToolAuthorization::Deny { reason } => reason,
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
        Arc::new(GeneralToolPolicyAdapter::new(DenyAllGeneral)),
    ];
    let policies = audited("plan_capability", run_id, audit, policies);
    Arc::new(ComposedToolAuthorizer::new(
        policies,
        Arc::new(AllowAllAuthorizer),
    ))
}

pub(crate) fn build_authorizer(
    run_id: &str,
    session_workdir: AbsolutePath,
    temporary_workspace: AbsolutePath,
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
                    operations: all_file,
                    scope: PathScope::Recursive(temporary_workspace),
                    description: "trusted temporary workspace".to_owned(),
                },
                FileRule {
                    effect: RuleEffect::Allow,
                    operations: read_only,
                    scope: PathScope::Recursive(session_workdir),
                    description: "trusted session workdir read".to_owned(),
                },
            ],
        })),
        Arc::new(ShellToolPolicyAdapter::new(ShellRuleSet::default())),
        Arc::new(GeneralToolPolicyAdapter::new(GeneralRuleSet::default())),
    ];
    let policies = audited("build_trusted_rules", run_id, audit.clone(), policies);
    let final_authorizer: Arc<dyn ToolAuthorizer> = match approval_mode {
        ApprovalMode::Ask => approval_authorizer,
        ApprovalMode::Auto => Arc::new(AuditedAllowAll {
            run_id: run_id.to_owned(),
            audit,
        }),
    };
    Arc::new(ComposedToolAuthorizer::new(policies, final_authorizer))
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
    use std::{num::NonZeroU64, time::Duration};

    use agent_tools::{
        Dispatcher, SessionPathResolver, ShellExecTool, ShellExecToolConfig, ShellFuture,
        ShellOutputSink, ShellRequest, ShellTool, ShellToolError, ToolRegistry,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct NeverShell;

    impl ShellTool for NeverShell {
        fn exec<'a>(
            &'a self,
            _request: ShellRequest,
            _sink: ShellOutputSink,
            _cancellation: CancellationToken,
        ) -> ShellFuture<'a> {
            Box::pin(std::future::ready(Err(ShellToolError::Cancelled)))
        }
    }

    fn shell_batch(workdir: &AbsolutePath, command: &str) -> agent_tools::ResolvedToolBatch {
        let mut registry = ToolRegistry::new();
        registry
            .register(ShellExecTool::new(
                Arc::new(NeverShell),
                SessionPathResolver::new(workdir.clone()),
                ShellExecToolConfig::new(
                    Duration::from_secs(120),
                    Duration::from_secs(600),
                    NonZeroU64::new(1024).expect("non-zero"),
                )
                .expect("config"),
            ))
            .expect("register shell");
        Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[ToolCall {
                id: ToolCallId::new("call_1").expect("call id"),
                name: ToolName::new("shell").expect("tool name"),
                arguments: serde_json::json!({"command": command}),
            }],
        )
    }

    #[test]
    fn allow_rule_wins_when_allow_and_deny_both_match() {
        let workdir = AbsolutePath::new(std::env::temp_dir()).expect("absolute temp");
        let rules = ShellRuleSet {
            rules: vec![
                ShellRule {
                    effect: RuleEffect::Deny,
                    command: CommandPattern::Prefix("git ".to_owned()),
                    workdir: None,
                    description: "deny git".to_owned(),
                },
                ShellRule {
                    effect: RuleEffect::Allow,
                    command: CommandPattern::Exact("git status".to_owned()),
                    workdir: None,
                    description: "allow status".to_owned(),
                },
            ],
        };
        let batch = shell_batch(&workdir, "git status");
        let Some(agent_tools::ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("shell resolves");
        };
        let facts = invocation
            .facts::<ShellAuthorizationFacts>()
            .expect("shell facts");
        assert_eq!(
            rules.evaluate(facts, invocation, &batch),
            PolicyEvaluation::Decide(ToolAuthorization::Allow)
        );
    }

    #[test]
    fn plan_file_policy_allows_only_read_scope_and_temporary_workspace_mutations() {
        let root = tempfile::tempdir().expect("root");
        let session = AbsolutePath::new(root.path().join("project")).expect("session path");
        let temporary = AbsolutePath::new(root.path().join("plans")).expect("temporary path");
        let batch = shell_batch(&session, "pwd");
        let Some(agent_tools::ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("shell resolves");
        };
        let policy = PlanFilePolicy {
            session_workdir: session.clone(),
            temporary_workspace: temporary.clone(),
        };

        let evaluate = |operation, path| {
            policy.evaluate(
                &FileAuthorizationFacts { operation, path },
                invocation,
                &batch,
            )
        };
        assert_eq!(
            evaluate(
                FileOperation::Read,
                AbsolutePath::new(session.as_path().join("src/lib.rs")).expect("absolute"),
            ),
            PolicyEvaluation::Decide(ToolAuthorization::Allow)
        );
        assert!(matches!(
            evaluate(
                FileOperation::Write,
                AbsolutePath::new(session.as_path().join("src/lib.rs")).expect("absolute"),
            ),
            PolicyEvaluation::Decide(ToolAuthorization::Deny { .. })
        ));
        assert_eq!(
            evaluate(
                FileOperation::Write,
                AbsolutePath::new(temporary.as_path().join("plan.md")).expect("absolute"),
            ),
            PolicyEvaluation::Decide(ToolAuthorization::Allow)
        );
    }
}
