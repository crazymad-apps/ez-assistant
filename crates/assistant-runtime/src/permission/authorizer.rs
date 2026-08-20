//! Runtime 权威工具授权器。
//!
//! Run 冻结变体、审批方式和作用域；权限文件本身在每次调用时重新从 Registry
//! 取得完整快照，因此显式 reload 会影响活动 Run 的后续 Tool Call。

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use agent_core::{
    AuthorizationFuture, PolicyEvaluation, ToolAuthorization, ToolAuthorizer, ToolPolicy,
};
use agent_tools::{
    AbsolutePath, FileAuthorizationFacts, FileBatchAuthorizationFacts, FileOperation,
    ResolvedToolBatch, ResolvedToolInvocation,
};
use assistant_protocol::{AgentVariant, ApprovalDecision, ApprovalId, ApprovalMode};

use super::{
    PermissionCoordinator, PermissionFileScope,
    matcher::{InvocationFactKind, fact_kind, file_matcher_matches, matches_rule},
};
use crate::{RuntimeError, RuntimeResult, SessionExecutionEnvironment};

pub(crate) type ApprovalFuture<'a> = Pin<Box<dyn Future<Output = ApprovalResolution> + Send + 'a>>;

#[derive(Clone, Debug)]
pub(crate) struct ApprovalResolution {
    authorization: ToolAuthorization,
    recheck_ask_rules: bool,
}

impl ApprovalResolution {
    pub(crate) fn allowed(_approval_id: ApprovalId, _decision: ApprovalDecision) -> Self {
        Self {
            authorization: ToolAuthorization::Allow,
            recheck_ask_rules: false,
        }
    }

    pub(crate) fn allowed_by_rule(_approval_id: ApprovalId) -> Self {
        Self {
            authorization: ToolAuthorization::Allow,
            recheck_ask_rules: true,
        }
    }

    pub(crate) fn user_denied(_approval_id: ApprovalId, _decision: ApprovalDecision) -> Self {
        Self {
            authorization: deny("tool call was denied by the user"),
            recheck_ask_rules: false,
        }
    }

    pub(crate) fn cancelled(_approval_id: ApprovalId, reason: &'static str) -> Self {
        Self {
            authorization: deny(reason),
            recheck_ask_rules: false,
        }
    }

    pub(crate) fn denied(reason: &'static str) -> Self {
        Self {
            authorization: deny(reason),
            recheck_ask_rules: false,
        }
    }
}

/// 审批等待接缝。具体 pending 状态与用户决策由 Runtime 实现，Core 只观察最终授权结果。
pub(crate) trait PermissionApprovalResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        batch: &'a ResolvedToolBatch,
    ) -> ApprovalFuture<'a>;
}

pub(crate) struct RuntimeToolAuthorizer {
    variant: AgentVariant,
    approval_mode: ApprovalMode,
    permission_scopes: Vec<PermissionFileScope>,
    permission_coordinator: Arc<PermissionCoordinator>,
    infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
    private_roots: Vec<AbsolutePath>,
    approval_resolver: Arc<dyn PermissionApprovalResolver>,
}

#[derive(Clone)]
pub(crate) struct RunAuthorizationScope {
    pub(crate) variant: AgentVariant,
    pub(crate) approval_mode: ApprovalMode,
}

impl RuntimeToolAuthorizer {
    pub(crate) fn new(
        scope: RunAuthorizationScope,
        permission_scopes: Vec<PermissionFileScope>,
        permission_coordinator: Arc<PermissionCoordinator>,
        infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
        environment: &SessionExecutionEnvironment,
        approval_resolver: Arc<dyn PermissionApprovalResolver>,
    ) -> RuntimeResult<Self> {
        let mut private_roots = vec![absolute(&environment.session_private_directory)?];
        if let Some(workspace_private_directory) = &environment.workspace_private_directory {
            private_roots.push(absolute(workspace_private_directory)?);
        }
        Ok(Self {
            variant: scope.variant,
            approval_mode: scope.approval_mode,
            permission_scopes,
            permission_coordinator,
            infrastructure_policies,
            private_roots,
            approval_resolver,
        })
    }

    /// 把单个活动子任务的 OS 临时目录加入 Plan 可写私有根。
    ///
    /// 该目录只随本次子执行的 lease 存活，不进入 Session 环境或权限文件。
    pub(crate) fn with_additional_private_root(mut self, path: &str) -> RuntimeResult<Self> {
        self.private_roots.push(absolute(path)?);
        Ok(self)
    }

    async fn authorize_inner(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> ToolAuthorization {
        self.evaluate(invocation, batch).await
    }

    async fn evaluate(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> ToolAuthorization {
        // 授权器按解析后的结构化 facts 判断，未知 facts 默认拒绝；不能退回到工具名或原始
        // JSON 猜测权限，否则新工具可能意外绕过现有策略。
        if fact_kind(invocation) == InvocationFactKind::Unknown {
            return deny("tool authorization facts are unsupported");
        }

        // Plan 的私有目录限制属于不可被规则放宽的产品硬边界，必须先于用户规则判断。
        if self.variant == AgentVariant::Plan
            && let Some(facts) = invocation.facts::<FileAuthorizationFacts>()
            && is_mutation(facts.operation)
            && !self.plan_mutation_is_safe(facts.path.clone()).await
        {
            return deny(
                "Plan only permits structured file mutations inside Agent private directories",
            );
        }

        // Host policy 表达附件不可变等基础设施事实，同样优先于可编辑权限文件。
        for policy in &self.infrastructure_policies {
            if let PolicyEvaluation::Decide(ToolAuthorization::Deny { reason }) =
                policy.evaluate(invocation, batch)
            {
                return ToolAuthorization::Deny { reason };
            }
        }

        let loads = match self
            .permission_coordinator
            .snapshot(&self.permission_scopes)
        {
            Ok(loads) => loads,
            Err(_) => {
                return deny("permission rules are unavailable");
            }
        };
        if loads.iter().any(|load| !load.is_valid()) {
            return deny("permission rules are unavailable");
        }

        if let Some(facts) = invocation.facts::<FileBatchAuthorizationFacts>() {
            let mut path_effects = vec![(false, false, false); facts.paths.len()];
            for load in &loads {
                let Some(document) = &load.document else {
                    return deny("permission rules are unavailable");
                };
                for rule in &document.rules {
                    if !rule.variants.contains(&self.variant) {
                        continue;
                    }
                    let super::PermissionMatcher::File(matcher) = &rule.matcher else {
                        continue;
                    };
                    for (index, path) in facts.paths.iter().enumerate() {
                        if !file_matcher_matches(matcher, facts.operation, path) {
                            continue;
                        }
                        match rule.effect {
                            super::PermissionEffect::Deny => path_effects[index].0 = true,
                            super::PermissionEffect::Ask => path_effects[index].1 = true,
                            super::PermissionEffect::Allow => path_effects[index].2 = true,
                        }
                    }
                }
            }
            if path_effects.iter().any(|(denied, _, _)| *denied) {
                return deny("tool call is denied by a permission rule");
            }
            if path_effects.iter().any(|(_, asked, _)| *asked) {
                return self.resolve_and_recheck(invocation, batch).await;
            }
            if path_effects.iter().all(|(_, _, allowed)| *allowed) {
                return ToolAuthorization::Allow;
            }
            return match self.approval_mode {
                ApprovalMode::Ask => self.resolve_and_recheck(invocation, batch).await,
                ApprovalMode::Auto => ToolAuthorization::Allow,
            };
        }

        // 三层规则不是“最近一层覆盖上一层”：任意 Deny 胜出，其次 Ask，最后才是 Allow。
        // 这样局部 Allow 无法绕过更高层显式 Deny，规则合并结果也与遍历顺序无关。
        let mut deny_rule = false;
        let mut ask_rule = false;
        let mut allow_rule = false;
        for load in &loads {
            let Some(document) = &load.document else {
                return deny("permission rules are unavailable");
            };
            for rule in &document.rules {
                if !matches_rule(rule, self.variant, invocation) {
                    continue;
                }
                match rule.effect {
                    super::PermissionEffect::Deny => deny_rule = true,
                    super::PermissionEffect::Ask => ask_rule = true,
                    super::PermissionEffect::Allow => allow_rule = true,
                };
            }
        }
        if deny_rule {
            return deny("tool call is denied by a permission rule");
        }
        if ask_rule {
            return self.resolve_and_recheck(invocation, batch).await;
        }
        if allow_rule {
            return ToolAuthorization::Allow;
        }
        match self.approval_mode {
            ApprovalMode::Ask => self.resolve_and_recheck(invocation, batch).await,
            ApprovalMode::Auto => ToolAuthorization::Allow,
        }
    }

    async fn resolve_and_recheck(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
    ) -> ToolAuthorization {
        let resolution = self.approval_resolver.resolve(invocation, batch).await;
        if resolution.authorization != ToolAuthorization::Allow {
            return resolution.authorization;
        }
        // 审批可能等待很久。期间用户可以 reload 新规则，Session/Host 状态也可能变化；
        // 因此旧的 Allow 只是继续检查的许可，不能直接成为最终执行许可。
        if let Some(evaluation) = self
            .current_hard_or_rule_denial(invocation, batch, resolution.recheck_ask_rules)
            .await
        {
            evaluation
        } else {
            resolution.authorization
        }
    }

    async fn current_hard_or_rule_denial(
        &self,
        invocation: &ResolvedToolInvocation,
        batch: &ResolvedToolBatch,
        recheck_ask_rules: bool,
    ) -> Option<ToolAuthorization> {
        if self.variant == AgentVariant::Plan
            && let Some(facts) = invocation.facts::<FileAuthorizationFacts>()
            && is_mutation(facts.operation)
            && !self.plan_mutation_is_safe(facts.path.clone()).await
        {
            return Some(deny("tool call became denied while approval was pending"));
        }
        if self.infrastructure_policies.iter().any(|policy| {
            matches!(
                policy.evaluate(invocation, batch),
                PolicyEvaluation::Decide(ToolAuthorization::Deny { .. })
            )
        }) {
            return Some(deny("tool call became denied while approval was pending"));
        }
        let Ok(loads) = self
            .permission_coordinator
            .snapshot(&self.permission_scopes)
        else {
            return Some(deny(
                "permission rules became unavailable while approval was pending",
            ));
        };
        for load in loads {
            let Some(document) = &load.document else {
                return Some(deny(
                    "permission rules became unavailable while approval was pending",
                ));
            };
            if document.rules.iter().any(|rule| {
                (rule.effect == super::PermissionEffect::Deny
                    || (recheck_ask_rules && rule.effect == super::PermissionEffect::Ask))
                    && matches_rule(rule, self.variant, invocation)
            }) {
                return Some(deny("tool call became denied while approval was pending"));
            }
        }
        None
    }

    async fn plan_mutation_is_safe(&self, target: AbsolutePath) -> bool {
        let roots = self.private_roots.clone();
        tokio::task::spawn_blocking(move || {
            roots.iter().any(|root| {
                target.as_path().starts_with(root.as_path())
                    && physical_path_stays_within(target.as_path().to_path_buf(), root)
            })
        })
        .await
        .unwrap_or(false)
    }
}

impl ToolAuthorizer for RuntimeToolAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        Box::pin(self.authorize_inner(invocation, batch))
    }
}

fn absolute(path: &str) -> RuntimeResult<AbsolutePath> {
    AbsolutePath::new(path).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "session private directory",
    })
}

fn is_mutation(operation: FileOperation) -> bool {
    matches!(
        operation,
        FileOperation::Write | FileOperation::Edit | FileOperation::Delete
    )
}

/// 对目标或最近存在祖先做物理解析，拒绝私有目录内通过 symlink 逃逸的路径。
/// 同一用户并发替换路径仍不属于 OS 沙箱保证。
fn physical_path_stays_within(mut target: PathBuf, root: &AbsolutePath) -> bool {
    let Ok(canonical_root) = std::fs::canonicalize(root.as_path()) else {
        return false;
    };
    loop {
        match std::fs::symlink_metadata(&target) {
            Ok(_) => {
                return std::fs::canonicalize(&target)
                    .is_ok_and(|canonical| canonical.starts_with(&canonical_root));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !target.pop() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
}

fn deny(reason: &'static str) -> ToolAuthorization {
    ToolAuthorization::Deny {
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_core::{ToolAuthorization, ToolAuthorizer};
    use agent_tools::{
        Dispatcher, FileAuthorizationFacts, FileBatchAuthorizationFacts, FileOperation,
        ResolvedBatchItemRef, SessionPathResolver, Tool, ToolContext, ToolError, ToolExecuteFuture,
        ToolRegistry, ToolResolution,
    };
    use agent_types::{ToolCall, ToolCallId, ToolName};
    use assistant_protocol::{AgentVariant, ApprovalId, ApprovalMode};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::permission::{
        PermissionFileRevision, PermissionFileStore, VolatilePermissionFileStore,
    };

    #[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
    struct FileInput {
        path: String,
    }

    struct FileFactsTool {
        operation: FileOperation,
        resolver: SessionPathResolver,
    }

    #[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
    struct BatchFileInput {
        paths: Vec<String>,
    }

    struct BatchFileFactsTool {
        resolver: SessionPathResolver,
    }

    impl Tool for BatchFileFactsTool {
        type Input = BatchFileInput;
        type ResolvedInput = BatchFileInput;
        type Output = serde_json::Value;

        fn name(&self) -> ToolName {
            ToolName::new("inspect_images").expect("tool name")
        }

        fn description(&self) -> String {
            "test batch file facts".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            let paths = input
                .paths
                .iter()
                .map(|path| {
                    self.resolver
                        .resolve(path)
                        .map_err(|error| ToolError::invalid_input(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ToolResolution::with_facts(
                input,
                FileBatchAuthorizationFacts {
                    operation: FileOperation::Read,
                    paths: paths.clone(),
                },
                json!({"paths": paths, "operation": "read"}),
            ))
        }

        fn execute<'a>(
            &'a self,
            _input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(json!({"ok": true}))))
        }
    }

    impl Tool for FileFactsTool {
        type Input = FileInput;
        type ResolvedInput = FileInput;
        type Output = serde_json::Value;

        fn name(&self) -> ToolName {
            ToolName::new(match self.operation {
                FileOperation::Write => "write_file",
                FileOperation::Edit => "edit_file",
                FileOperation::Delete => "delete_file",
                _ => "read_file",
            })
            .expect("tool name")
        }

        fn description(&self) -> String {
            "test file facts".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            let path = self
                .resolver
                .resolve(&input.path)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            Ok(ToolResolution::with_facts(
                input,
                FileAuthorizationFacts {
                    operation: self.operation,
                    path: path.clone(),
                },
                json!({"path": path.as_path()}),
            ))
        }

        fn execute<'a>(
            &'a self,
            _input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(json!({"ok": true}))))
        }
    }

    struct StaticApproval(ToolAuthorization);

    impl PermissionApprovalResolver for StaticApproval {
        fn resolve<'a>(
            &'a self,
            _invocation: &'a ResolvedToolInvocation,
            _batch: &'a ResolvedToolBatch,
        ) -> ApprovalFuture<'a> {
            let resolution = if self.0 == ToolAuthorization::Allow {
                ApprovalResolution::allowed(
                    ApprovalId::new("approval-test").expect("approval id"),
                    ApprovalDecision::AllowOnce,
                )
            } else {
                ApprovalResolution::denied("scripted rejection")
            };
            Box::pin(std::future::ready(resolution))
        }
    }

    fn test_authorizer(
        variant: AgentVariant,
        approval_mode: ApprovalMode,
        permission_scopes: Vec<PermissionFileScope>,
        permission_coordinator: Arc<PermissionCoordinator>,
        infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
        environment: &SessionExecutionEnvironment,
        approval_resolver: Arc<dyn PermissionApprovalResolver>,
    ) -> RuntimeResult<RuntimeToolAuthorizer> {
        RuntimeToolAuthorizer::new(
            RunAuthorizationScope {
                variant,
                approval_mode,
            },
            permission_scopes,
            permission_coordinator,
            infrastructure_policies,
            environment,
            approval_resolver,
        )
    }

    #[tokio::test]
    async fn deny_and_ask_override_allow_and_auto() {
        let root = TempDir::new().expect("tempdir");
        let environment = environment(&root);
        let target = root.path().join("workspace/out.txt");
        let global = file_rule_document("allow", target.to_string_lossy().as_ref());
        let workspace = file_rule_document("ask", target.to_string_lossy().as_ref());
        let session = file_rule_document("deny", target.to_string_lossy().as_ref());
        let permission_coordinator = coordinator([
            (PermissionFileScope::Global, global),
            (workspace_scope(), workspace),
            (session_scope(), session),
        ])
        .await;
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Auto,
            scopes(),
            permission_coordinator,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .expect("authorizer");
        assert!(matches!(
            authorize_file(&authorizer, &environment, &target).await,
            ToolAuthorization::Deny { .. }
        ));

        let ask_coordinator = coordinator([
            (
                PermissionFileScope::Global,
                file_rule_document("allow", target.to_string_lossy().as_ref()),
            ),
            (
                workspace_scope(),
                file_rule_document("ask", target.to_string_lossy().as_ref()),
            ),
            (session_scope(), empty_document()),
        ])
        .await;
        let ask_authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Auto,
            scopes(),
            ask_coordinator,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Deny {
                reason: "scripted rejection".to_owned(),
            })),
        )
        .expect("ask authorizer");
        assert_eq!(
            authorize_file(&ask_authorizer, &environment, &target).await,
            ToolAuthorization::Deny {
                reason: "scripted rejection".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn plan_only_allows_structured_mutation_in_physical_private_roots() {
        let root = TempDir::new().expect("tempdir");
        let environment = environment(&root);
        let coordinator = coordinator([
            (PermissionFileScope::Global, empty_document()),
            (workspace_scope(), empty_document()),
            (session_scope(), empty_document()),
        ])
        .await;
        let plan = test_authorizer(
            AgentVariant::Plan,
            ApprovalMode::Auto,
            scopes(),
            coordinator.clone(),
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .expect("plan authorizer");
        let private_target =
            PathBuf::from(&environment.session_private_directory).join("analysis.js");
        assert_eq!(
            authorize_file(&plan, &environment, &private_target).await,
            ToolAuthorization::Allow
        );
        let workspace_target = root.path().join("workspace/product.rs");
        assert!(matches!(
            authorize_file(&plan, &environment, &workspace_target).await,
            ToolAuthorization::Deny { .. }
        ));

        let build = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Auto,
            scopes(),
            coordinator,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .expect("build authorizer");
        assert_eq!(
            authorize_file(&build, &environment, &workspace_target).await,
            ToolAuthorization::Allow
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = root.path().join("outside");
            std::fs::create_dir(&outside).expect("outside");
            let link = PathBuf::from(&environment.session_private_directory).join("escape");
            symlink(&outside, &link).expect("symlink");
            assert!(matches!(
                authorize_file(&plan, &environment, &link.join("escaped.txt")).await,
                ToolAuthorization::Deny { .. }
            ));
        }
    }

    #[tokio::test]
    async fn plan_child_can_write_only_inside_its_additional_private_root() {
        let root = TempDir::new().expect("tempdir");
        let environment = environment(&root);
        let child_private = root.path().join("child-private");
        std::fs::create_dir(&child_private).expect("child private directory");
        let coordinator = coordinator([
            (PermissionFileScope::Global, empty_document()),
            (workspace_scope(), empty_document()),
            (session_scope(), empty_document()),
        ])
        .await;
        let plan = test_authorizer(
            AgentVariant::Plan,
            ApprovalMode::Auto,
            scopes(),
            coordinator,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .expect("plan authorizer")
        .with_additional_private_root(child_private.to_string_lossy().as_ref())
        .expect("child private root");

        assert_eq!(
            authorize_file(&plan, &environment, &child_private.join("analysis.rs")).await,
            ToolAuthorization::Allow
        );
        assert!(matches!(
            authorize_file(&plan, &environment, &root.path().join("outside.rs")).await,
            ToolAuthorization::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn active_authorizer_reads_the_reloaded_registry_on_each_call() {
        let root = TempDir::new().expect("tempdir");
        let environment = environment(&root);
        let target = root.path().join("workspace/reload.txt");
        let scope = PermissionFileScope::Global;
        let store = Arc::new(VolatilePermissionFileStore::default());
        let revision = store
            .replace_permission_file(
                &scope,
                &PermissionFileRevision::Missing,
                file_rule_document("allow", target.to_string_lossy().as_ref()),
            )
            .await
            .expect("initial permission file");
        let coordinator =
            Arc::new(PermissionCoordinator::open(store.clone(), vec![scope.clone()]).await);
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Ask,
            vec![scope.clone()],
            coordinator.clone(),
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Deny {
                reason: "unexpected approval".to_owned(),
            })),
        )
        .expect("authorizer");
        assert_eq!(
            authorize_file(&authorizer, &environment, &target).await,
            ToolAuthorization::Allow
        );

        store
            .replace_permission_file(
                &scope,
                &revision,
                file_rule_document("deny", target.to_string_lossy().as_ref()),
            )
            .await
            .expect("replace permission file");
        let outcome = coordinator
            .reload(vec![scope])
            .await
            .expect("reload permissions");
        assert!(outcome.applied);
        assert!(matches!(
            authorize_file(&authorizer, &environment, &target).await,
            ToolAuthorization::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn batch_reads_combine_separate_allow_roots_and_deny_any_matching_path() {
        let root = TempDir::new().expect("tempdir");
        let environment = environment(&root);
        let workspace_image = root.path().join("workspace/a.png");
        let private_image = root.path().join("session-private/b.png");
        let paths = [&workspace_image, &private_image];
        let allowed = coordinator([
            (
                PermissionFileScope::Global,
                batch_read_rule_document(
                    [
                        ("allow-workspace", "allow", root.path().join("workspace")),
                        (
                            "allow-private",
                            "allow",
                            root.path().join("session-private"),
                        ),
                    ]
                    .as_slice(),
                ),
            ),
            (workspace_scope(), empty_document()),
            (session_scope(), empty_document()),
        ])
        .await;
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Ask,
            scopes(),
            allowed,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Deny {
                reason: "unexpected approval".to_owned(),
            })),
        )
        .expect("authorizer");
        assert_eq!(
            authorize_batch_read(&authorizer, &environment, &paths).await,
            ToolAuthorization::Allow
        );

        let denied = coordinator([
            (
                PermissionFileScope::Global,
                batch_read_rule_document(
                    [
                        ("allow-all", "allow", root.path().to_path_buf()),
                        ("deny-private", "deny", root.path().join("session-private")),
                    ]
                    .as_slice(),
                ),
            ),
            (workspace_scope(), empty_document()),
            (session_scope(), empty_document()),
        ])
        .await;
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Auto,
            scopes(),
            denied,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .expect("authorizer");
        assert!(matches!(
            authorize_batch_read(&authorizer, &environment, &paths).await,
            ToolAuthorization::Deny { .. }
        ));
    }

    async fn authorize_file(
        authorizer: &RuntimeToolAuthorizer,
        environment: &SessionExecutionEnvironment,
        target: &std::path::Path,
    ) -> ToolAuthorization {
        let mut registry = ToolRegistry::new();
        registry
            .register(FileFactsTool {
                operation: FileOperation::Write,
                resolver: SessionPathResolver::new(
                    AbsolutePath::new(&environment.working_directory).expect("working directory"),
                ),
            })
            .expect("register tool");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[ToolCall {
                id: ToolCallId::new("call-write").expect("call id"),
                name: ToolName::new("write_file").expect("tool name"),
                arguments: json!({"path": target}),
            }],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("batch item") else {
            panic!("file call resolves");
        };
        authorizer.authorize(invocation, &batch).await
    }

    async fn authorize_batch_read(
        authorizer: &RuntimeToolAuthorizer,
        environment: &SessionExecutionEnvironment,
        paths: &[&std::path::PathBuf],
    ) -> ToolAuthorization {
        let mut registry = ToolRegistry::new();
        registry
            .register(BatchFileFactsTool {
                resolver: SessionPathResolver::new(
                    AbsolutePath::new(&environment.working_directory).expect("working directory"),
                ),
            })
            .expect("register tool");
        let batch = Dispatcher::resolve_batch(
            &registry.snapshot(),
            &[ToolCall {
                id: ToolCallId::new("call-inspect").expect("call id"),
                name: ToolName::new("inspect_images").expect("tool name"),
                arguments: json!({"paths": paths}),
            }],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("batch item") else {
            panic!("batch read resolves");
        };
        authorizer.authorize(invocation, &batch).await
    }

    async fn coordinator<const N: usize>(
        documents: [(PermissionFileScope, Vec<u8>); N],
    ) -> Arc<PermissionCoordinator> {
        let store = Arc::new(VolatilePermissionFileStore::default());
        let scopes = documents
            .iter()
            .map(|(scope, _)| scope.clone())
            .collect::<Vec<_>>();
        for (scope, document) in documents {
            store
                .replace_permission_file(&scope, &PermissionFileRevision::Missing, document)
                .await
                .expect("write permission file");
        }
        Arc::new(PermissionCoordinator::open(store, scopes).await)
    }

    fn environment(root: &TempDir) -> SessionExecutionEnvironment {
        let workspace = root.path().join("workspace");
        let workspace_private = root.path().join("workspace-private");
        let attachments = root.path().join("attachments");
        let tool_images = root.path().join("tool-images");
        let session_private = root.path().join("session-private");
        for directory in [
            &workspace,
            &workspace_private,
            &attachments,
            &tool_images,
            &session_private,
        ] {
            std::fs::create_dir(directory).expect("directory");
        }
        SessionExecutionEnvironment {
            workspace_id: Some(
                assistant_protocol::WorkspaceId::new("w-test").expect("workspace id"),
            ),
            working_directory: workspace.to_string_lossy().into_owned(),
            workspace_private_directory: Some(workspace_private.to_string_lossy().into_owned()),
            session_attachment_directory: attachments.to_string_lossy().into_owned(),
            session_tool_image_directory: tool_images.to_string_lossy().into_owned(),
            session_private_directory: session_private.to_string_lossy().into_owned(),
        }
    }

    fn scopes() -> Vec<PermissionFileScope> {
        vec![
            PermissionFileScope::Global,
            workspace_scope(),
            session_scope(),
        ]
    }

    fn workspace_scope() -> PermissionFileScope {
        PermissionFileScope::Workspace(
            assistant_protocol::WorkspaceId::new("w-test").expect("workspace id"),
        )
    }

    fn session_scope() -> PermissionFileScope {
        PermissionFileScope::Session(
            assistant_protocol::SessionId::new("s-test").expect("session id"),
        )
    }

    fn empty_document() -> Vec<u8> {
        br#"{"schema_version":1,"rules":[]}"#.to_vec()
    }

    fn file_rule_document(effect: &str, path: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "rules": [{
                "id": format!("{effect}-write"),
                "effect": effect,
                "variants": ["build"],
                "matcher": {
                    "type": "file",
                    "operation": "write",
                    "path": path,
                    "path_match": "exact"
                }
            }]
        }))
        .expect("permission JSON")
    }

    fn batch_read_rule_document(rules: &[(&str, &str, PathBuf)]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "rules": rules.iter().map(|(id, effect, path)| json!({
                "id": id,
                "effect": effect,
                "variants": ["build"],
                "matcher": {
                    "type": "file",
                    "operation": "read",
                    "path": path,
                    "path_match": "recursive"
                }
            })).collect::<Vec<_>>()
        }))
        .expect("permission JSON")
    }
}
