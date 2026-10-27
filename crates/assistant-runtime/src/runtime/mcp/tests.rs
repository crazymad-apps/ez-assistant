//! 配置命令提交任务的所有权边界；不启动 Host 或使用真实文件。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use assistant_protocol::{
    McpConfigurationMutation, McpServerKey, McpServerSource, McpServerTarget,
};
use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::{
    ConfigDocument, ConfigSourceFuture, ConfigSourceLoad, ConfigSourceReplace,
    ConfigSourceReplaceFuture, McpConfigSource,
};

#[test]
fn connection_test_preserves_safe_source_and_configuration_reason() {
    let message =
        "来源：插件 · general-assistant；缺少插件原生配置文件 deepseek-search.json，请填写 api_key";
    let error =
        crate::McpConnectionError::new(McpConnectionFailureKind::InvalidConfig, message.to_owned());
    let result = connection_failure(
        McpServerKey::new("deepseek-search").unwrap(),
        &error,
        Instant::now(),
    );
    let diagnostic = result.diagnostic.unwrap();
    assert_eq!(diagnostic.message, message);
    assert_eq!(diagnostic.code, McpDiagnosticCode::InvalidConfig);
}

#[derive(Default)]
struct PausingSource {
    entered: Notify,
    release: Notify,
    saved: Mutex<Option<String>>,
}

impl McpConfigSource for PausingSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(async { ConfigSourceLoad::Missing })
    }

    fn replace(&self, expected: Option<String>, value: String) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(async move {
            assert!(expected.is_none());
            self.entered.notify_one();
            self.release.notified().await;
            *self.saved.lock().unwrap() = Some(value.clone());
            ConfigSourceReplace::Applied(ConfigDocument::new(value, "committed".to_owned()))
        })
    }
}

#[tokio::test]
async fn disconnected_configuration_caller_does_not_drop_inflight_file_commit() {
    let source = Arc::new(PausingSource::default());
    let store = Arc::new(crate::mcp::McpConfigStore::new(source.clone()));
    let tasks = Arc::new(super::super::tasks::RuntimeTasks::new());
    let key = McpServerKey::new("fixture").unwrap();
    let definition = crate::ManagedMcpServerDefinition {
        source: McpServerSource::Builtin,
        server_key: key.clone(),
        definition_digest: "fixture-definition".to_owned(),
        source_enabled: true,
        source_available: true,
        ready: true,
        raw: json!({"command":"fixture","enabled":false})
            .as_object()
            .unwrap()
            .clone(),
        parameter_bindings: BTreeMap::new(),
        parameters: vec![],
        plugin_settings_revision: None,
        plugin_sha256: None,
        configuration: None,
    };
    let request = MutateMcpConfigurationRequest {
        expected_revision: "absent".to_owned(),
        mutation: McpConfigurationMutation::PatchManaged {
            target: McpServerTarget {
                source: McpServerSource::Builtin,
                server_key: key,
            },
            expected_definition_digest: "fixture-definition".to_owned(),
            enabled: Some(false),
            environment: BTreeMap::new(),
            headers: BTreeMap::new(),
        },
    };
    let caller = tokio::spawn({
        let tasks = tasks.clone();
        let store = store.clone();
        async move {
            commit_managed_configuration(
                tasks.as_ref(),
                store,
                definition,
                request,
                None,
                crate::observation::ObservationCoordinator::new(8),
            )
            .await
        }
    });
    source.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(
        store.gate.try_lock().is_err(),
        "commit task still owns the file gate"
    );
    source.release.notify_one();
    tasks.close();
    assert!(tasks.wait_or_abort(Duration::from_secs(1)).await);
    let document: serde_json::Value =
        serde_json::from_str(source.saved.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(
        document["managedServers"]["builtin"]["fixture"]["enabled"],
        false
    );
    assert!(store.gate.try_lock().is_ok());
}
