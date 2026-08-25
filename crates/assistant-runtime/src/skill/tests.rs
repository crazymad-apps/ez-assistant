use super::*;
use agent_tools::{Tool, ToolContext};
use agent_types::ToolCallId;

fn catalog_with(mut skill: SkillCandidate) -> SessionSkillCatalog {
    skill.definition_digest = format!("sha256-v1:{}", "1".repeat(64));
    SessionSkillCatalog::from_discovery(SkillDiscovery {
        status: SkillDiscoveryStatus::Available,
        candidates: vec![skill.clone()],
        winners: vec![skill],
        diagnostics: Vec::new(),
    })
    .expect("catalog")
}

fn candidate(name: &str, source: SkillSource, path: &str) -> SkillCandidate {
    SkillCandidate {
        name: SkillName::parse(name).expect("valid name"),
        description: format!("{name} description"),
        source,
        source_path: path.to_owned(),
        definition_digest: format!("digest-{path}"),
        body: "instructions".to_owned(),
        metadata: SkillMetadata::default(),
        model_invocable: true,
        user_invocable: true,
    }
}

#[test]
fn name_validation_matches_stable_agent_skills_subset() {
    for valid in ["a", "review-pr", "skill2"] {
        assert_eq!(SkillName::parse(valid).expect("valid").as_str(), valid);
    }
    for invalid in ["", "Review", "-review", "review-", "review--pr", "审查"] {
        assert!(SkillName::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn source_priority_and_disabled_name_are_deterministic_without_fallback() {
    let scan = SkillScanResult {
        candidates: vec![
            candidate("review", SkillSource::UserAgents, "/u/.agents/review"),
            candidate(
                "review",
                SkillSource::WorkspaceEzAssistant,
                "/w/.ez-assistant/review",
            ),
            candidate("write", SkillSource::UserEzAssistant, "/u/.ez/write"),
        ],
        diagnostics: Vec::new(),
        complete: true,
    };
    let enabled = compile_skill_discovery(scan.clone(), &[]);
    assert_eq!(enabled.winners.len(), 2);
    assert_eq!(enabled.winners[0].source, SkillSource::WorkspaceEzAssistant);

    let disabled = compile_skill_discovery(
        scan,
        &[SkillNameState {
            name: SkillName::parse("review").expect("name"),
            enabled: false,
            updated_at_ms: 1,
        }],
    );
    assert_eq!(
        disabled
            .winners
            .iter()
            .map(|winner| winner.name.as_str())
            .collect::<Vec<_>>(),
        vec!["write"]
    );
    assert!(
        disabled
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == SkillDiagnosticCode::DisabledByName })
    );
}

#[test]
fn same_source_conflict_has_no_winner_and_input_order_does_not_matter() {
    let first = SkillScanResult {
        candidates: vec![
            candidate("review", SkillSource::WorkspaceAgents, "/w/a"),
            candidate("review", SkillSource::WorkspaceAgents, "/w/b"),
        ],
        diagnostics: Vec::new(),
        complete: true,
    };
    let mut reversed = first.clone();
    reversed.candidates.reverse();
    let first = compile_skill_discovery(first, &[]);
    let reversed = compile_skill_discovery(reversed, &[]);
    assert_eq!(first, reversed);
    assert!(first.winners.is_empty());
    assert_eq!(
        first.diagnostics[0].code,
        SkillDiagnosticCode::SameSourceConflict
    );
}

#[test]
fn incomplete_scan_never_exposes_partial_winners() {
    let discovery = compile_skill_discovery(
        SkillScanResult {
            candidates: vec![candidate(
                "review",
                SkillSource::WorkspaceAgents,
                "/w/review",
            )],
            diagnostics: Vec::new(),
            complete: false,
        },
        &[],
    );
    assert_eq!(discovery.status, SkillDiscoveryStatus::Unavailable);
    assert!(discovery.winners.is_empty());
}

#[test]
fn frozen_catalog_revision_ignores_shared_source_paths_and_prompt_is_safe_and_deterministic() {
    let discovery = compile_skill_discovery(
        SkillScanResult {
            candidates: vec![SkillCandidate {
                description: "Review <carefully> & report".to_owned(),
                definition_digest: format!("sha256-v1:{}", "1".repeat(64)),
                ..candidate(
                    "review",
                    SkillSource::WorkspaceEzAssistant,
                    "/workspace/skill",
                )
            }],
            diagnostics: Vec::new(),
            complete: true,
        },
        &[],
    );
    let catalog = SessionSkillCatalog::from_discovery(discovery).expect("catalog");
    catalog.validate_structure().expect("valid catalog");
    assert_eq!(catalog.definitions[0].source_path, "/workspace/skill");
    let relocated = SessionSkillCatalog::from_discovery(compile_skill_discovery(
        SkillScanResult {
            candidates: vec![SkillCandidate {
                description: "Review <carefully> & report".to_owned(),
                definition_digest: format!("sha256-v1:{}", "1".repeat(64)),
                ..candidate(
                    "review",
                    SkillSource::WorkspaceEzAssistant,
                    "/relocated/workspace/skill",
                )
            }],
            diagnostics: Vec::new(),
            complete: true,
        },
        &[],
    ))
    .expect("relocated catalog");
    assert_eq!(catalog.revision, relocated.revision);
    let prompt = catalog.render_system_prompt_part();
    assert!(prompt.contains("description=\"Review &lt;carefully&gt; &amp; report\""));
    assert!(!prompt.contains("/workspace/"));
}

#[test]
fn legacy_and_empty_catalogs_have_a_stable_empty_revision() {
    let legacy = SessionSkillCatalog::legacy_unavailable();
    let empty = SessionSkillCatalog::from_discovery(SkillDiscovery {
        status: SkillDiscoveryStatus::Available,
        candidates: Vec::new(),
        winners: Vec::new(),
        diagnostics: Vec::new(),
    })
    .expect("empty catalog");
    assert_eq!(legacy.revision, empty.revision);
    assert_eq!(empty.status, SkillCatalogStatus::Empty);
    assert_eq!(legacy.status, SkillCatalogStatus::LegacyUnavailable);
    assert!(
        serde_json::from_str::<SessionSkillCatalog>(
            &serde_json::to_string(&legacy).expect("serialize")
        )
        .expect("deserialize")
        .validate_structure()
        .is_ok()
    );
}

#[tokio::test]
async fn load_skill_reports_staged_already_active_and_stable_failures() {
    let catalog = catalog_with(candidate(
        "review",
        SkillSource::WorkspaceEzAssistant,
        "/workspace/review",
    ));
    let latch = std::sync::Arc::new(SkillActivationLatch::new(Vec::new()));
    let tool = LoadSkillTool::new(catalog, latch.clone());
    let invoke = |name: &str, call_id: &str| {
        let input = tool
            .resolve(LoadSkillInput {
                name: name.to_owned(),
            })
            .expect("resolve")
            .into_input();
        let context =
            ToolContext::default().with_call_id(ToolCallId::new(call_id).expect("call id"));
        (input, context)
    };
    let (input, context) = invoke("review", "call-1");
    assert_eq!(
        tool.execute(input, context).await.expect("execute").status,
        LoadSkillStatus::Staged
    );
    let (input, context) = invoke("review", "call-2");
    assert_eq!(
        tool.execute(input, context).await.expect("execute").status,
        LoadSkillStatus::AlreadyActive
    );
    latch
        .commit(&[ToolCallId::new("call-1").expect("call id")])
        .expect("commit staged activation");
    let (input, context) = invoke("review", "call-3");
    assert_eq!(
        tool.execute(input, context).await.expect("execute").status,
        LoadSkillStatus::AlreadyActive
    );
    let (input, context) = invoke("missing", "call-4");
    assert_eq!(
        tool.execute(input, context).await.expect("execute").status,
        LoadSkillStatus::NotFound
    );

    let mut not_invocable = candidate(
        "private",
        SkillSource::WorkspaceEzAssistant,
        "/workspace/private",
    );
    not_invocable.model_invocable = false;
    let private = LoadSkillTool::new(
        catalog_with(not_invocable),
        std::sync::Arc::new(SkillActivationLatch::new(Vec::new())),
    );
    let resolved = private
        .resolve(LoadSkillInput {
            name: "private".to_owned(),
        })
        .expect("resolve");
    assert_eq!(
        private
            .execute(
                resolved.into_input(),
                ToolContext::default().with_call_id(ToolCallId::new("call-5").expect("call id")),
            )
            .await
            .expect("execute")
            .status,
        LoadSkillStatus::NotModelInvocable
    );

    let unavailable = LoadSkillTool::new(
        SessionSkillCatalog::legacy_unavailable(),
        std::sync::Arc::new(SkillActivationLatch::new(Vec::new())),
    );
    let resolved = unavailable
        .resolve(LoadSkillInput {
            name: "review".to_owned(),
        })
        .expect("resolve");
    assert_eq!(
        unavailable
            .execute(
                resolved.into_input(),
                ToolContext::default().with_call_id(ToolCallId::new("call-6").expect("call id")),
            )
            .await
            .expect("execute")
            .status,
        LoadSkillStatus::CatalogUnavailable
    );
}

#[test]
fn load_skill_definition_is_stable_and_uses_a_plain_string_name() {
    let tool = LoadSkillTool::new(
        SessionSkillCatalog::legacy_unavailable(),
        std::sync::Arc::new(SkillActivationLatch::new(Vec::new())),
    );
    let mut registry = agent_tools::ToolRegistry::new();
    registry.register(tool).expect("register");
    let snapshot = registry.snapshot();
    let definition = &snapshot.definitions()[0];
    assert_eq!(definition.name.as_str(), "load_skill");
    assert_eq!(
        definition.input_schema["properties"]["name"]["type"],
        "string"
    );
    assert!(
        definition.input_schema["properties"]["name"]
            .get("enum")
            .is_none()
    );
}

#[test]
fn parent_and_child_activation_latches_do_not_share_active_state() {
    let mut catalog = catalog_with(candidate(
        "review",
        SkillSource::WorkspaceEzAssistant,
        "/workspace/review",
    ));
    let definition = catalog.definitions.remove(0);
    let parent = SkillActivationLatch::new(Vec::new());
    let child = SkillActivationLatch::new(Vec::new());
    assert!(
        parent
            .stage(
                ToolCallId::new("parent-call").expect("call id"),
                definition.clone(),
            )
            .expect("stage parent")
    );
    assert!(
        child
            .stage(ToolCallId::new("child-call").expect("call id"), definition,)
            .expect("stage child")
    );
}
