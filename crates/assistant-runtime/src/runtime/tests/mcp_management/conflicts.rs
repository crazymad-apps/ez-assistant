//! 同名来源的管理恢复：真实命令入口，只用内存配置与 Volatile Store。

use super::*;
use crate::{plugin::*, storage::*};
use assistant_protocol::{IdempotencyKey, McpServerSource, PluginPackageSelection};
use futures_util::future::BoxFuture;

struct CollisionFiles(Arc<MemoryMcpSource>);

impl PluginFilePort for CollisionFiles {
    fn mcp_candidates(
        &self,
        _: Vec<StoredPlugin>,
    ) -> BoxFuture<'_, Result<PluginMcpDiscovery, PluginFileError>> {
        Box::pin(async {
            Ok(PluginMcpDiscovery {
                definitions: vec![crate::ManagedMcpServerDefinition {
                    source: McpServerSource::Plugin("example".into()),
                    server_key: McpServerKey::new("fixture").unwrap(),
                    definition_digest: "fixture-definition".into(),
                    source_enabled: true,
                    source_available: true,
                    ready: true,
                    raw: json!({"command":"plugin-fixture","enabled":true})
                        .as_object()
                        .unwrap()
                        .clone(),
                    parameter_bindings: BTreeMap::new(),
                    parameters: vec![],
                    plugin_settings_revision: None,
                    plugin_sha256: Some("a".repeat(64)),
                    configuration: Some(self.0.clone()),
                }],
                ..Default::default()
            })
        })
    }
    fn prepare(
        &self,
        _: IdempotencyKey,
        _: PluginPackageSelection,
        _: CancellationToken,
        _: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> BoxFuture<'_, Result<PreparedPluginPackage, PluginFileError>> {
        unreachable!("no installation")
    }
    fn publish(
        &self,
        _: IdempotencyKey,
        _: PluginPackageSelection,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        unreachable!("no installation")
    }
    fn discard(
        &self,
        _: IdempotencyKey,
        _: Option<PluginPackageSelection>,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        unreachable!("no installation")
    }
    fn finish(
        &self,
        _: IdempotencyKey,
        _: CommittedPluginChange,
    ) -> BoxFuture<'_, Result<(), PluginFileError>> {
        unreachable!("no installation")
    }
    fn audit(
        &self,
        _: Vec<StoredPlugin>,
        _: Vec<StoredPluginOperation>,
    ) -> BoxFuture<'_, Result<PluginAudit, PluginFileError>> {
        Box::pin(async { Ok(PluginAudit::default()) })
    }
}

async fn collision_fixture() -> (AssistantRuntime, Arc<MemoryMcpSource>, Arc<MemoryMcpSource>) {
    let user = Arc::new(MemoryMcpSource(Mutex::new(("r1".into(), json!({
        "mcpServers":{"fixture":{"command":"user-fixture","enabled":true},"other":{"command":"other","enabled":false}},
        "managedServers":{"builtin":{"unrelated":{"enabled":false}}}
    }).to_string()))));
    let plugin = Arc::new(MemoryMcpSource(Mutex::new((
        "p1".into(),
        json!({"servers":{"fixture":{"env":{"CUSTOM":"plugin-value"}}}}).to_string(),
    ))));
    let runtime = runtime(empty_model())
        .with_plugin_files(Arc::new(CollisionFiles(plugin.clone())))
        .with_mcp_services(
            user.clone(),
            Arc::new(crate::mcp::UnavailableMcpConnectionFactory),
            Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
        );
    let mut operation = StoredPluginOperation {
        key: IdempotencyKey::new("collision-fixture").unwrap(),
        kind: PluginOperationKind::Install,
        intent_json: "{}".into(),
        phase: PluginOperationPhase::Accepted,
        committed_json: "[]".into(),
        cancel_requested: false,
        error_code: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    runtime
        .store
        .accept_plugin_operation(operation.clone())
        .await
        .unwrap();
    for phase in [
        PluginOperationPhase::Preparing,
        PluginOperationPhase::Committing,
        PluginOperationPhase::Cleaning,
    ] {
        let expected_phase = operation.phase;
        operation.phase = phase;
        let change = if phase == PluginOperationPhase::Cleaning {
            PluginRecordChange::Upsert(StoredPlugin {
                name: "example".into(),
                source: PluginSource::Upload,
                active_version: Some("1.0.0".into()),
                active_sha256: Some("a".repeat(64)),
                active_manifest_json: Some(json!({"mcp":["fixture"]}).to_string()),
                enabled: true,
                parameters_json: "{}".into(),
                software_paths_json: "{}".into(),
                market_status: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
        } else {
            PluginRecordChange::Keep
        };
        runtime
            .store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase,
                next: operation.clone(),
                plugin: change,
            })
            .await
            .unwrap();
    }
    (runtime, user, plugin)
}

#[tokio::test]
async fn preferred_user_mcp_can_be_disabled_or_removed_without_changing_plugin() {
    for remove in [false, true] {
        let (runtime, user, plugin) = collision_fixture().await;
        let plugin_before = plugin.document();
        let installed_before = runtime.store.list_plugins().await.unwrap();
        let before: serde_json::Value = serde_json::from_str(&user.document()).unwrap();
        let snapshot = runtime
            .get_mcp_configuration(GetMcpConfigurationRequest::default())
            .await
            .unwrap()
            .snapshot;
        let key = McpServerKey::new("fixture").unwrap();
        assert_eq!(
            snapshot
                .servers
                .iter()
                .filter(|server| server.server_key == key && server.enabled)
                .count(),
            2
        );
        assert!(
            !snapshot
                .diagnostics
                .iter()
                .any(|item| item.server_key.as_ref() == Some(&key))
        );
        let result = runtime
            .mutate_mcp_configuration(MutateMcpConfigurationRequest {
                expected_revision: snapshot.revision.clone(),
                mutation: if remove {
                    McpConfigurationMutation::Remove {
                        server_key: key.clone(),
                    }
                } else {
                    McpConfigurationMutation::SetEnabled {
                        server_key: key.clone(),
                        enabled: false,
                    }
                },
            })
            .await
            .unwrap()
            .snapshot;
        let saved: serde_json::Value = serde_json::from_str(&user.document()).unwrap();
        if remove {
            assert!(saved["mcpServers"].get("fixture").is_none());
        } else {
            assert_eq!(saved["mcpServers"]["fixture"]["enabled"], false);
        }
        assert_eq!(saved["mcpServers"]["other"], before["mcpServers"]["other"]);
        assert_eq!(saved["managedServers"], before["managedServers"]);
        assert_eq!(plugin.document(), plugin_before);
        assert_eq!(
            runtime.store.list_plugins().await.unwrap(),
            installed_before
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|item| item.server_key.as_ref() == Some(&key))
        );
        assert!(result.servers.iter().any(|server| server.server_key == key
            && matches!(server.source, McpServerSource::Plugin(_))
            && server.enabled));
        assert!(
            runtime
                .mcp_service
                .registry
                .catalog_snapshot()
                .unwrap()
                .is_empty()
        );
        // 解除名称冲突不会绕过内容 CAS；删除后也不能通过用户命令修改仅存的插件条目。
        assert!(matches!(
            runtime
                .mutate_mcp_configuration(MutateMcpConfigurationRequest {
                    expected_revision: snapshot.revision,
                    mutation: McpConfigurationMutation::Remove {
                        server_key: key.clone()
                    },
                })
                .await,
            Err(RuntimeError::McpConfigConflict)
        ));
        if remove {
            assert!(matches!(
                runtime
                    .mutate_mcp_configuration(MutateMcpConfigurationRequest {
                        expected_revision: result.revision,
                        mutation: McpConfigurationMutation::SetEnabled {
                            server_key: key,
                            enabled: false
                        },
                    })
                    .await,
                Err(RuntimeError::McpConfigInvalid)
            ));
            assert_eq!(plugin.document(), plugin_before);
        }
    }
}

#[tokio::test]
async fn user_mcp_can_be_created_imported_and_edited_over_same_name_plugin() {
    let (runtime, user, plugin) = collision_fixture().await;
    let plugin_before = plugin.document();
    let mut snapshot = runtime
        .get_mcp_configuration(Default::default())
        .await
        .unwrap()
        .snapshot;
    let key = McpServerKey::new("fixture").unwrap();
    snapshot = runtime
        .mutate_mcp_configuration(MutateMcpConfigurationRequest {
            expected_revision: snapshot.revision,
            mutation: McpConfigurationMutation::Remove {
                server_key: key.clone(),
            },
        })
        .await
        .unwrap()
        .snapshot;
    let document = json!({"mcpServers":{"fixture":{"command":"user-command"}}}).to_string();
    let preview = runtime
        .preview_mcp_import(assistant_protocol::PreviewMcpImportRequest {
            document: document.clone(),
        })
        .await
        .unwrap();
    assert!(!preview.entries[0].conflicts_with_existing);
    snapshot = runtime
        .mutate_mcp_configuration(MutateMcpConfigurationRequest {
            expected_revision: snapshot.revision,
            mutation: McpConfigurationMutation::Import {
                document,
                replace_server_keys: vec![],
            },
        })
        .await
        .unwrap()
        .snapshot;
    let mutation = serde_json::from_value(json!({"type":"upsert","payload":{"server":{
        "server_key":"fixture","display_name":"My service","description":"User override","enabled":true,
        "transport":{"type":"stdio","payload":{"command":{"mode":"replace","value":"changed-command"}}}
    }}})).unwrap();
    let snapshot = runtime
        .mutate_mcp_configuration(MutateMcpConfigurationRequest {
            expected_revision: snapshot.revision,
            mutation,
        })
        .await
        .unwrap()
        .snapshot;
    assert!(
        !snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.server_key.as_ref() == Some(&key))
    );
    let managed = snapshot
        .servers
        .iter()
        .find(|server| server.source == McpServerSource::Plugin("example".into()))
        .unwrap();
    assert_eq!(managed.shadowed_by, Some(McpServerSource::User));
    let saved: serde_json::Value = serde_json::from_str(&user.document()).unwrap();
    assert_eq!(saved["mcpServers"]["fixture"]["command"], "changed-command");
    assert_eq!(plugin.document(), plugin_before);
}
