use super::*;
use crate::permission::{
    FilePermissionMatcher, PathMatch, PermissionDocument, PermissionEffect,
    PermissionFileOperation, PermissionMatcher, PermissionRule, PermissionRuleSource,
    PermissionStoreFuture,
};
use std::sync::RwLock;

struct Defaults(RwLock<PermissionDocument>);

impl PermissionRuleSource for Defaults {
    fn load(&self) -> PermissionStoreFuture<'_, PermissionDocument> {
        let document = self.0.read().unwrap().clone();
        Box::pin(async move { Ok(document) })
    }
}

fn denied_tree(
    path: &std::path::Path,
    operation: PermissionFileOperation,
    excluded: Vec<String>,
) -> PermissionDocument {
    PermissionDocument {
        schema_version: 1,
        rules: vec![PermissionRule {
            id: "default-deny".into(),
            effect: PermissionEffect::Deny,
            variants: vec![AgentVariant::Build, AgentVariant::Plan],
            matcher: PermissionMatcher::File(FilePermissionMatcher {
                path: path.to_str().unwrap().into(),
                path_match: PathMatch::Recursive,
                operation,
                excluded_paths: excluded,
            }),
        }],
    }
}

#[tokio::test]
async fn injected_denies_merge_with_user_rules_and_auto_for_single_and_batch_calls() {
    let root = TempDir::new().unwrap();
    let environment = environment(&root);
    let shared = root.path().join("shared");
    let own = shared.join("own");
    std::fs::create_dir_all(&own).unwrap();
    let source = Arc::new(Defaults(RwLock::new(denied_tree(
        &shared,
        PermissionFileOperation::Write,
        vec![own.to_str().unwrap().into()],
    ))));
    let allowed = shared.join("SKILL.md");
    let permissions = coordinator([(
        PermissionFileScope::Global,
        file_rule_document("allow", allowed.to_str().unwrap()),
    )])
    .await;
    let authorizer = test_authorizer(
        AgentVariant::Build,
        ApprovalMode::Auto,
        vec![PermissionFileScope::Global],
        permissions,
        Vec::new(),
        &environment,
        Arc::new(StaticApproval(ToolAuthorization::Allow)),
    )
    .unwrap()
    .with_default_rules(Some(source.clone()));
    assert_eq!(
        authorize_file(&authorizer, &environment, &allowed).await,
        deny("tool call is denied by a permission rule")
    );
    assert_eq!(
        authorize_file(&authorizer, &environment, &own.join("SKILL.md")).await,
        ToolAuthorization::Allow
    );
    assert_eq!(
        authorize_file_operation(&authorizer, &environment, &allowed, FileOperation::Read).await,
        ToolAuthorization::Allow
    );

    // 同一活动授权器读取当前规则，批量图片不能因为其中一张在允许目录就越过拒绝。
    *source.0.write().unwrap() = denied_tree(
        &shared,
        PermissionFileOperation::Read,
        vec![own.to_str().unwrap().into()],
    );
    assert_eq!(
        authorize_batch_read(
            &authorizer,
            &environment,
            &[&own.join("a.png"), &shared.join("b.png")]
        )
        .await,
        deny("tool call is denied by a permission rule")
    );
    assert_eq!(
        authorize_batch_read(&authorizer, &environment, &[&own.join("a.png")]).await,
        ToolAuthorization::Allow
    );
}

#[cfg(unix)]
#[tokio::test]
async fn injected_and_user_denies_both_match_physical_aliases() {
    let root = TempDir::new().unwrap();
    let environment = environment(&root);
    let protected = root.path().join("protected");
    std::fs::create_dir(&protected).unwrap();
    let alias = root.path().join("workspace/alias");
    std::os::unix::fs::symlink(&protected, &alias).unwrap();
    for injected in [true, false] {
        let document = denied_tree(
            &std::fs::canonicalize(&protected).unwrap(),
            PermissionFileOperation::Write,
            Vec::new(),
        );
        let user_document = if injected {
            PermissionDocument::empty()
        } else {
            document.clone()
        };
        let permissions =
            coordinator([(PermissionFileScope::Global, user_document.render().unwrap())]).await;
        let source = injected
            .then(|| Arc::new(Defaults(RwLock::new(document))) as Arc<dyn PermissionRuleSource>);
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Auto,
            vec![PermissionFileScope::Global],
            permissions,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(ToolAuthorization::Allow)),
        )
        .unwrap()
        .with_default_rules(source);
        assert_eq!(
            authorize_file(&authorizer, &environment, &alias.join("new/SKILL.md")).await,
            deny("tool call is denied by a permission rule")
        );
    }
}

struct ChangeRulesDuringApproval {
    source: Arc<Defaults>,
    target: PathBuf,
}

#[tokio::test]
async fn a_default_exception_still_requires_normal_permission() {
    let root = TempDir::new().unwrap();
    let environment = environment(&root);
    let own = root.path().join("outside-own");
    let target = own.join("SKILL.md");
    let source = Arc::new(Defaults(RwLock::new(denied_tree(
        root.path(),
        PermissionFileOperation::Write,
        vec![own.to_str().unwrap().into()],
    ))));
    for explicit_deny in [false, true] {
        let document = if explicit_deny {
            file_rule_document("deny", target.to_str().unwrap())
        } else {
            empty_document()
        };
        let permissions = coordinator([(PermissionFileScope::Global, document)]).await;
        let authorizer = test_authorizer(
            AgentVariant::Build,
            ApprovalMode::Ask,
            vec![PermissionFileScope::Global],
            permissions,
            Vec::new(),
            &environment,
            Arc::new(StaticApproval(deny("scripted rejection"))),
        )
        .unwrap()
        .with_default_rules(Some(source.clone()));
        assert_eq!(
            authorize_file(&authorizer, &environment, &target).await,
            deny(if explicit_deny {
                "tool call is denied by a permission rule"
            } else {
                "scripted rejection"
            })
        );
    }
}
impl PermissionApprovalResolver for ChangeRulesDuringApproval {
    fn resolve<'a>(
        &'a self,
        _: &'a ResolvedToolInvocation,
        _: &'a ResolvedToolBatch,
        _: ApprovalMode,
    ) -> ApprovalFuture<'a> {
        Box::pin(async move {
            *self.source.0.write().unwrap() =
                denied_tree(&self.target, PermissionFileOperation::Write, Vec::new());
            ApprovalResolution::allowed(
                ApprovalId::new("approved").unwrap(),
                ApprovalDecision::AllowOnce,
            )
        })
    }
}

#[tokio::test]
async fn newly_injected_denial_is_rechecked_after_user_approval() {
    let root = TempDir::new().unwrap();
    let environment = environment(&root);
    let target = root.path().join("outside/secret.key");
    let source = Arc::new(Defaults(RwLock::new(PermissionDocument::empty())));
    let permissions = coordinator([(PermissionFileScope::Global, empty_document())]).await;
    let authorizer = test_authorizer(
        AgentVariant::Build,
        ApprovalMode::Ask,
        vec![PermissionFileScope::Global],
        permissions,
        Vec::new(),
        &environment,
        Arc::new(ChangeRulesDuringApproval {
            source: source.clone(),
            target: target.clone(),
        }),
    )
    .unwrap()
    .with_default_rules(Some(source));
    assert_eq!(
        authorize_file(&authorizer, &environment, &target).await,
        deny("tool call became denied while approval was pending")
    );
}

#[test]
fn injected_exceptions_cannot_be_silently_saved_as_broader_user_rules() {
    let root = TempDir::new().unwrap();
    let document = denied_tree(
        root.path(),
        PermissionFileOperation::Write,
        vec![root.path().join("own").to_str().unwrap().into()],
    );
    document.validate().unwrap();
    assert!(document.render().is_err());
    let mut json: serde_json::Value =
        serde_json::from_slice(&file_rule_document("deny", root.path().to_str().unwrap())).unwrap();
    json["rules"][0]["matcher"]["excluded_paths"] = json!([]);
    assert!(PermissionDocument::parse(&serde_json::to_vec(&json).unwrap()).is_err());
}
