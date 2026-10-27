use std::sync::Mutex;

use assistant_protocol::{
    McpFieldChange, McpServerDraft, McpServerTarget, McpServerTransportDraft,
};
use serde_json::json;

use super::*;
use crate::{ConfigDocument, ConfigSourceFuture, ConfigSourceReplaceFuture};

#[derive(Default)]
struct Source(Mutex<Option<String>>);

impl Source {
    fn text(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }
    fn revision(value: &str) -> String {
        format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
    }
}

impl McpConfigSource for Source {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(async {
            match self.text() {
                Some(value) => ConfigSourceLoad::Document(ConfigDocument::new(
                    value.clone(),
                    Self::revision(&value),
                )),
                None => ConfigSourceLoad::Missing,
            }
        })
    }
    fn replace(&self, expected: Option<String>, value: String) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(async move {
            let mut current = self.0.lock().unwrap();
            if current.as_deref().map(Self::revision) != expected {
                return ConfigSourceReplace::Conflict(ConfigSourceLoad::Missing);
            }
            *current = Some(value.clone());
            ConfigSourceReplace::Applied(ConfigDocument::new(value.clone(), Self::revision(&value)))
        })
    }
}

fn definition(source: Arc<Source>) -> ManagedMcpServerDefinition {
    ManagedMcpServerDefinition {
        source: McpServerSource::Plugin("example".to_owned()),
        server_key: McpServerKey::new("search").unwrap(),
        definition_digest: "definition-1".to_owned(),
        source_enabled: true,
        source_available: true,
        ready: true,
        raw: json!({"command":"fixture", "env":{"DEFAULT":"package", "BOUND":"plugin-parameter"}})
            .as_object()
            .unwrap()
            .clone(),
        parameter_bindings: BTreeMap::from([("BOUND".to_owned(), vec!["token".to_owned()])]),
        parameters: vec![PluginConfigurationFieldSnapshot {
            name: "token".to_owned(),
            label: "Token".to_owned(),
            required: true,
            secret: true,
            is_set: true,
        }],
        plugin_settings_revision: Some("parameters-1".to_owned()),
        plugin_sha256: Some("a".repeat(64)),
        configuration: Some(source),
    }
}

#[test]
fn invalid_managed_configuration_names_source_and_missing_required_fields() {
    let mut definition = definition(Arc::new(Source::default()));
    definition.ready = false;
    definition.parameters[0].is_set = false;
    let diagnostic = invalid(&definition);
    assert!(diagnostic.message.contains("来源：插件 · example"));
    assert!(diagnostic.message.contains("缺少必填插件配置项：token"));
    assert!(!diagnostic.message.contains("plugin-parameter"));
    definition.source_available = false;
    assert!(invalid(&definition).message.contains("服务来源不可用"));
}

fn patch(definition: &ManagedMcpServerDefinition, values: Value) -> McpConfigurationMutation {
    McpConfigurationMutation::PatchManaged {
        target: McpServerTarget {
            source: definition.source.clone(),
            server_key: definition.server_key.clone(),
        },
        expected_definition_digest: definition.definition_digest.clone(),
        enabled: None,
        environment: serde_json::from_value(values).unwrap(),
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn plugin_file_is_single_authority_with_null_reset_and_cas() {
    let user = Arc::new(Source::default());
    let plugin = Arc::new(Source::default());
    let store = McpConfigStore::new(user.clone());
    let definition = definition(plugin.clone());
    let mutation = patch(
        &definition,
        json!({"TOKEN":{"mode":"replace","value":"private-user-token"},"DEFAULT":{"mode":"remove"}}),
    );
    store
        .mutate_managed("absent", &mutation, &definition)
        .await
        .unwrap();
    assert!(user.text().is_none());
    let saved: Value = serde_json::from_str(&plugin.text().unwrap()).unwrap();
    assert_eq!(saved["servers"]["search"]["env"]["DEFAULT"], Value::Null);
    let (snapshot, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(snapshot.revision, "absent");
    let metadata = snapshot.servers[0].managed.as_ref().unwrap();
    assert!(!metadata.environment["DEFAULT"]);
    assert!(metadata.environment["TOKEN"]);
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private-user-token")
    );
    assert!(
        candidate.servers[&definition.server_key]
            .removed_environment()
            .contains("DEFAULT")
    );
    assert!(matches!(
        store.mutate_managed("absent", &mutation, &definition).await,
        Err(RuntimeError::McpConfigConflict)
    ));
    let reset = McpConfigurationMutation::ResetManaged {
        target: McpServerTarget {
            source: definition.source.clone(),
            server_key: definition.server_key.clone(),
        },
        expected_definition_digest: definition.definition_digest.clone(),
        enabled: false,
        environment: vec!["DEFAULT".to_owned()],
        headers: vec![],
    };
    store
        .mutate_managed(&metadata.revision, &reset, &definition)
        .await
        .unwrap();
    let (_, candidate) = store
        .resolve_managed(&[definition], &[], &BTreeMap::new())
        .await
        .unwrap();
    let server = candidate.servers.values().next().unwrap();
    assert!(!server.removed_environment().contains("DEFAULT"));
    if let super::super::McpServerTransportConfig::Stdio { environment, .. } = server.transport() {
        assert_eq!(environment["DEFAULT"].expose(), "package");
    } else {
        panic!("stdio");
    }
}

#[tokio::test]
async fn managed_mutations_reject_bound_reserved_cross_transport_and_startup_fields() {
    let source = Arc::new(Source::default());
    let definition = definition(source.clone());
    let store = McpConfigStore::new(Arc::new(Source::default()));
    for key in [
        "BOUND",
        "EZ_ASSISTANT_PLUGIN_ROOT",
        "ez_assistant_plugin_data_dir",
        "BAD=NAME",
    ] {
        for mode in ["keep", "remove"] {
            let mutation = patch(&definition, json!({key:{"mode":mode}}));
            assert!(matches!(
                store.mutate_managed("absent", &mutation, &definition).await,
                Err(RuntimeError::McpConfigInvalid)
            ));
        }
    }
    let mut invalid = patch(&definition, json!({}));
    if let McpConfigurationMutation::PatchManaged { headers, .. } = &mut invalid {
        headers.insert("Authorization".to_owned(), McpSecretChange::Remove);
    }
    assert!(
        store
            .mutate_managed("absent", &invalid, &definition)
            .await
            .is_err()
    );
    assert!(source.text().is_none());
    *source.0.lock().unwrap() = Some(json!({"servers":{"search":{"command":"evil"}}}).to_string());
    let (snapshot, candidate) = store
        .resolve_managed(&[definition], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(!snapshot.diagnostics.is_empty());
    assert!(candidate.servers.is_empty());
    assert_eq!(candidate.blocked_keys.len(), 1);
}

#[tokio::test]
async fn missing_parameters_and_disabled_sources_remain_visible_without_false_connection() {
    let source = Arc::new(Source::default());
    let mut definition = definition(source.clone());
    definition.ready = false;
    let store = McpConfigStore::new(Arc::new(Source::default()));
    let (snapshot, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(snapshot.servers.len(), 1);
    assert!(
        !snapshot.servers[0]
            .managed
            .as_ref()
            .unwrap()
            .configuration_ready
    );
    assert!(candidate.servers.is_empty());
    definition.source_enabled = false;
    let mut change = patch(
        &definition,
        json!({"TOKEN":{"mode":"replace","value":"user"}}),
    );
    if let McpConfigurationMutation::PatchManaged { enabled, .. } = &mut change {
        *enabled = Some(true);
    }
    store
        .mutate_managed("absent", &change, &definition)
        .await
        .unwrap();
    let (snapshot, candidate) = store
        .resolve_managed(&[definition], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(!snapshot.servers[0].enabled);
    assert!(
        snapshot.servers[0]
            .managed
            .as_ref()
            .unwrap()
            .configured_enabled
    );
    assert_eq!(
        snapshot.servers[0].runtime_state,
        McpServerRuntimeState::Disabled
    );
    assert!(candidate.servers.is_empty());
}

#[tokio::test]
async fn builtin_key_save_does_not_enable_or_overwrite_other_fields_on_import() {
    let user = Arc::new(Source::default());
    let store = McpConfigStore::new(user.clone());
    let mut definition = definition(Arc::new(Source::default()));
    definition.source = McpServerSource::Builtin;
    definition.configuration = None;
    definition.parameter_bindings.clear();
    definition
        .raw
        .insert("enabled".to_owned(), Value::Bool(false));
    store
        .mutate_managed(
            "absent",
            &patch(
                &definition,
                json!({"KEY":{"mode":"replace","value":"builtin-private"}}),
            ),
            &definition,
        )
        .await
        .unwrap();
    let (before, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(!before.servers[0].enabled);
    assert!(!candidate.servers[&definition.server_key].enabled());
    let imported=McpConfigurationMutation::Import { document:json!({"mcpServers":{},"managedServers":{"builtin":{"search":{"enabled":true}}},"unrelated":true}).to_string(),replace_server_keys:vec![] };
    store
        .mutate_with_runtime(&before.revision, imported, &BTreeMap::new())
        .await
        .unwrap();
    let (after, _) = store
        .resolve_managed(&[definition], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(!after.servers[0].enabled);
    assert!(after.servers[0].managed.as_ref().unwrap().environment["KEY"]);
    let saved: Value = serde_json::from_str(&user.text().unwrap()).unwrap();
    assert_eq!(saved["unrelated"], true);
}

#[tokio::test]
async fn user_source_wins_without_conflict_and_disabled_source_does_not_claim_connection() {
    let user = Arc::new(Source(Mutex::new(Some(
        json!({"mcpServers":{"search":{"command":"independent"}}}).to_string(),
    ))));
    let store = McpConfigStore::new(user);
    let mut definition = definition(Arc::new(Source::default()));
    let (_, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.blocked_keys.is_empty());
    definition.source_enabled = false;
    let (snapshot, candidate) = store
        .resolve_managed(&[definition], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.blocked_keys.is_empty());
    assert_eq!(snapshot.servers.len(), 2);
    assert_eq!(
        candidate.servers.values().next().unwrap().source(),
        &McpServerSource::User
    );
}

#[tokio::test]
async fn stale_file_write_conflicts_and_noop_does_not_rewrite() {
    let plugin = Arc::new(Source::default());
    let store = McpConfigStore::new(Arc::new(Source::default()));
    let definition = definition(plugin.clone());
    let first = patch(
        &definition,
        json!({"FIRST":{"mode":"replace","value":"one"}}),
    );
    let second = patch(
        &definition,
        json!({"SECOND":{"mode":"replace","value":"two"}}),
    );
    let (a, b) = tokio::join!(
        store.mutate_managed("absent", &first, &definition),
        store.mutate_managed("absent", &second, &definition)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (snapshot, _) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    let revision = &snapshot.servers[0].managed.as_ref().unwrap().revision;
    let before = plugin.text();
    let winner = if a.is_ok() { first } else { second };
    store
        .mutate_managed(revision, &winner, &definition)
        .await
        .unwrap();
    assert_eq!(before, plugin.text());
}

#[tokio::test]
async fn invalid_record_isolated_and_test_draft_never_persists() {
    let plugin=Arc::new(Source(Mutex::new(Some(json!({"servers":{"search":{"enabled":"invalid"},"other":{"env":{"A":"valid"}}},"custom":"keep"}).to_string()))));
    let store = McpConfigStore::new(Arc::new(Source::default()));
    let bad = definition(plugin.clone());
    let mut other = bad.clone();
    other.server_key = McpServerKey::new("other").unwrap();
    let (snapshot, candidate) = store
        .resolve_managed(&[bad, other.clone()], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(candidate.servers.len(), 1);
    assert_eq!(snapshot.servers.len(), 2);
    let revision = snapshot
        .servers
        .iter()
        .find(|server| server.server_key == other.server_key)
        .unwrap()
        .managed
        .as_ref()
        .unwrap()
        .revision
        .clone();
    let before = plugin.text();
    let config = store
        .managed_test_config(
            &assistant_protocol::McpManagedServerTestDraft {
                target: McpServerTarget {
                    source: other.source.clone(),
                    server_key: other.server_key.clone(),
                },
                expected_revision: revision,
                expected_definition_digest: other.definition_digest.clone(),
                environment: serde_json::from_value(
                    json!({"A":{"mode":"replace","value":"draft"}}),
                )
                .unwrap(),
                headers: BTreeMap::new(),
            },
            &other,
        )
        .await
        .unwrap();
    assert!(config.enabled());
    assert_eq!(before, plugin.text());
}

#[tokio::test]
async fn independent_service_still_resolves_original_draft() {
    let store = McpConfigStore::new(Arc::new(Source::default()));
    let draft = McpServerDraft {
        server_key: McpServerKey::new("custom").unwrap(),
        display_name: "Custom".to_owned(),
        description: String::new(),
        enabled: true,
        transport: McpServerTransportDraft::Stdio {
            command: McpFieldChange::Replace("fixture".to_owned()),
            args: McpFieldChange::Keep,
            cwd: McpFieldChange::Keep,
            environment: BTreeMap::new(),
        },
        startup_timeout_ms: None,
        tool_timeout_ms: None,
    };
    assert_eq!(
        store.resolve_draft(&draft).await.unwrap().source(),
        &McpServerSource::User
    );
}

#[tokio::test]
async fn removing_last_plugin_service_keeps_user_file_and_reports_orphaned_settings() {
    let plugin = Arc::new(Source(Mutex::new(Some(
        json!({"servers":{"removed":{"env":{"KEY":"private-retained"}}}}).to_string(),
    ))));
    let store = McpConfigStore::new(Arc::new(Source::default()));
    let before = plugin.text();
    let sources = vec![(
        McpServerSource::Plugin("example".to_owned()),
        plugin.clone() as Arc<dyn McpConfigSource>,
    )];
    let (snapshot, candidate) = store
        .resolve_managed(&[], &sources, &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.servers.is_empty());
    assert!(snapshot.servers.is_empty());
    assert!(snapshot.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .server_key
            .as_ref()
            .is_some_and(|key| key.as_str() == "removed")
    }));
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("private-retained")
    );
    assert_eq!(plugin.text(), before);
}

#[tokio::test]
async fn priority_is_stable_and_disabled_sources_fall_through_without_rewriting_files() {
    let user = Arc::new(Source(Mutex::new(Some(
        json!({"mcpServers":{"search":{"command":"user"}}}).to_string(),
    ))));
    let store = McpConfigStore::new(user.clone());
    let mut a = definition(Arc::new(Source::default()));
    a.source = McpServerSource::Plugin("a-plugin".into());
    let mut z = definition(Arc::new(Source::default()));
    z.source = McpServerSource::Plugin("z-plugin".into());
    let mut builtin = definition(Arc::new(Source::default()));
    builtin.source = McpServerSource::Builtin;
    builtin.configuration = None;
    let key = a.server_key.clone();
    for (stage, expected) in [
        McpServerSource::User,
        a.source.clone(),
        z.source.clone(),
        McpServerSource::Builtin,
    ]
    .into_iter()
    .enumerate()
    {
        if stage > 0 {
            *user.0.lock().unwrap() = Some(
                json!({"mcpServers":{"search":{"command":"user","enabled":false}}}).to_string(),
            );
        }
        if stage > 1 {
            a.source_enabled = false;
        }
        if stage > 2 {
            z.raw.insert("enabled".into(), Value::Bool(false));
        }
        let before = user.text();
        for definitions in [
            vec![builtin.clone(), z.clone(), a.clone()],
            vec![a.clone(), z.clone(), builtin.clone()],
        ] {
            let (snapshot, candidate) = store
                .resolve_managed(&definitions, &[], &BTreeMap::new())
                .await
                .unwrap();
            assert_eq!(candidate.servers[&key].source(), &expected);
            assert!(candidate.blocked_keys.is_empty());
            assert!(snapshot.diagnostics.is_empty());
            for server in snapshot.servers {
                assert_eq!(
                    server.shadowed_by,
                    (server.enabled && server.source != expected).then(|| expected.clone())
                );
                if server.shadowed_by.is_some() || !server.enabled {
                    assert!(!server.needs_refresh);
                }
            }
        }
        assert_eq!(user.text(), before);
    }
    builtin.raw.insert("enabled".into(), Value::Bool(false));
    let (_, candidate) = store
        .resolve_managed(&[a, z, builtin], &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(!candidate.servers[&key].enabled());
    assert!(candidate.blocked_keys.is_empty());
}

#[tokio::test]
async fn shadowed_source_keeps_only_its_own_runtime_state_until_refresh() {
    let user = Arc::new(Source(Mutex::new(Some(
        json!({"mcpServers":{"search":{"command":"user"}}}).to_string(),
    ))));
    let store = McpConfigStore::new(user);
    let definition = definition(Arc::new(Source::default()));
    for source in [definition.source.clone(), McpServerSource::User] {
        let runtime = BTreeMap::from([(
            definition.server_key.clone(),
            McpRegistryServerProjection {
                source: source.clone(),
                fingerprint: [0; 32],
                state: McpServerRuntimeState::Connected,
                tool_count: 7,
            },
        )]);
        let (snapshot, _) = store
            .resolve_managed(std::slice::from_ref(&definition), &[], &runtime)
            .await
            .unwrap();
        let plugin = snapshot
            .servers
            .iter()
            .find(|s| s.source == definition.source)
            .unwrap();
        assert_eq!(plugin.shadowed_by, Some(McpServerSource::User));
        assert_eq!(plugin.needs_refresh, source == definition.source);
        assert_eq!(
            plugin.tool_count,
            if source == definition.source { 7 } else { 0 }
        );
        assert_eq!(
            plugin.runtime_state == McpServerRuntimeState::Connected,
            source == definition.source
        );
    }
}

#[tokio::test]
async fn invalid_lower_source_does_not_block_winner_but_invalid_winner_does_not_fall_back() {
    let user = Arc::new(Source(Mutex::new(Some(
        json!({"mcpServers":{"search":{"command":"user"}}}).to_string(),
    ))));
    let store = McpConfigStore::new(user.clone());
    let mut definition = definition(Arc::new(Source::default()));
    definition.ready = false;
    let (snapshot, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.blocked_keys.is_empty());
    assert_eq!(
        candidate.servers[&definition.server_key].source(),
        &McpServerSource::User
    );
    assert!(!snapshot.diagnostics.is_empty()); // 真正的配置错误仍保留。
    *user.0.lock().unwrap() =
        Some(json!({"mcpServers":{"search":{"command":"user","enabled":false}}}).to_string());
    let (_, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.blocked_keys.contains(&definition.server_key));
    assert!(!candidate.servers.contains_key(&definition.server_key));
}

#[tokio::test]
async fn explicitly_disabled_invalid_user_entry_does_not_shadow_plugin() {
    let source = Arc::new(Source(Mutex::new(Some(
        json!({"mcpServers":{"search":{"command":42,"enabled":false}}}).to_string(),
    ))));
    let store = McpConfigStore::new(source);
    let definition = definition(Arc::new(Source::default()));
    let (_, candidate) = store
        .resolve_managed(std::slice::from_ref(&definition), &[], &BTreeMap::new())
        .await
        .unwrap();
    assert!(candidate.blocked_keys.is_empty());
    assert_eq!(
        candidate.servers[&definition.server_key].source(),
        &definition.source
    );
}
