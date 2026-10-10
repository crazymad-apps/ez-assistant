use std::{
    collections::VecDeque,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::*;
use crate::mcp::{
    McpConnectionFuture, McpRawCallResult, McpRawContent, McpSecret, McpServerTransportConfig,
    UnavailableMcpImageMaterializer,
};
use agent_tools::{Dispatcher, ResolvedBatchItemRef, ToolContext, ToolRegistry};
use agent_types::{ToolCall, ToolCallId, ToolName, ToolResultStatus};

struct FixtureConnection {
    pages: StdMutex<VecDeque<Vec<McpToolDefinition>>>,
    calls: StdMutex<VecDeque<Result<McpRawCallResult, McpConnectionError>>>,
    closed: AtomicUsize,
    close_failed: AtomicBool,
    block_call: AtomicBool,
    call_started: Notify,
}

impl FixtureConnection {
    fn new(tools: Vec<McpToolDefinition>) -> Arc<Self> {
        Arc::new(Self {
            pages: StdMutex::new(VecDeque::from([tools])),
            calls: StdMutex::new(VecDeque::new()),
            closed: AtomicUsize::new(0),
            close_failed: AtomicBool::new(false),
            block_call: AtomicBool::new(false),
            call_started: Notify::new(),
        })
    }

    fn with_call(self: &Arc<Self>, call: McpRawCallResult) {
        self.calls.lock().expect("calls").push_back(Ok(call));
    }

    fn with_failure(self: &Arc<Self>, kind: McpConnectionFailureKind) {
        self.calls
            .lock()
            .expect("calls")
            .push_back(Err(McpConnectionError::new(kind, "fixture request failed")));
    }
}

impl McpConnection for FixtureConnection {
    fn list_tools_page(
        &self,
        _cursor: Option<String>,
        _cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, super::super::McpToolPage> {
        Box::pin(async move {
            let tools = self
                .pages
                .lock()
                .expect("pages")
                .pop_front()
                .unwrap_or_default();
            Ok(super::super::McpToolPage {
                tools,
                next_cursor: None,
            })
        })
    }

    fn call_tool_once(
        &self,
        _tool_name: String,
        _arguments: Map<String, Value>,
        _cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, McpRawCallResult> {
        Box::pin(async move {
            if self.block_call.load(Ordering::SeqCst) {
                self.call_started.notify_one();
                std::future::pending::<()>().await;
            }
            self.calls
                .lock()
                .expect("calls")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(McpConnectionError::new(
                        McpConnectionFailureKind::ToolCall,
                        "fixture call is unavailable",
                    ))
                })
        })
    }

    fn close(&self, _cancellation: CancellationToken) -> McpConnectionFuture<'_, ()> {
        Box::pin(async move {
            self.closed.fetch_add(1, Ordering::SeqCst);
            if self.close_failed.load(Ordering::SeqCst) {
                return Err(McpConnectionError::new(
                    McpConnectionFailureKind::Close,
                    "fixture cleanup failed",
                ));
            }
            Ok(())
        })
    }
}

struct FixtureFactory {
    connections: StdMutex<VecDeque<Arc<FixtureConnection>>>,
}

impl FixtureFactory {
    fn new(connections: Vec<Arc<FixtureConnection>>) -> Arc<Self> {
        Arc::new(Self {
            connections: StdMutex::new(connections.into()),
        })
    }
}

impl McpConnectionFactory for FixtureFactory {
    fn connect(
        &self,
        _server: McpServerConfig,
        _options: McpConnectionOptions,
        _cancellation: CancellationToken,
    ) -> super::super::McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        Box::pin(async move {
            self.connections
                .lock()
                .expect("connections")
                .pop_front()
                .map(|connection| connection as Arc<dyn McpConnection>)
                .ok_or_else(|| {
                    McpConnectionError::new(
                        McpConnectionFailureKind::Connect,
                        "fixture connection is unavailable",
                    )
                })
        })
    }
}

fn tool(schema: Value) -> McpToolDefinition {
    McpToolDefinition {
        name: "create_issue".to_owned(),
        title: Some("Create issue".to_owned()),
        description: Some("Creates one issue".to_owned()),
        input_schema: schema,
        output_schema: Some(json!({
            "type": "object",
            "required": ["id"],
            "properties": {"id": {"type": "integer"}}
        })),
        annotations: Some(json!({"destructiveHint": true})),
    }
}

fn server(fingerprint: u8) -> McpServerConfig {
    McpServerConfig {
        plugin_name: None,
        plugin_sha256: None,
        server_key: McpServerKey::new("github").expect("key"),
        display_name: "GitHub".to_owned(),
        description: "Issues".to_owned(),
        enabled: true,
        transport: McpServerTransportConfig::Stdio {
            command: "fixture".to_owned(),
            args: Vec::new(),
            cwd: None,
            environment: BTreeMap::<String, McpSecret>::new(),
        },
        startup_timeout: None,
        tool_timeout: None,
        fingerprint: [fingerprint; 32],
    }
}

fn candidate(server: McpServerConfig) -> McpRegistryCandidate {
    let key = server.server_key.clone();
    McpRegistryCandidate {
        blocked_keys: BTreeSet::new(),
        supplemental_diagnostics: Vec::new(),
        document_valid: true,
        configured_keys: BTreeSet::from([key.clone()]),
        servers: BTreeMap::from([(key, server)]),
        diagnostics: Vec::new(),
    }
}

#[tokio::test]
async fn single_request_failures_keep_connection_for_next_explicit_call() {
    for kind in [
        McpConnectionFailureKind::ToolCall,
        McpConnectionFailureKind::Cancelled,
        McpConnectionFailureKind::UnsupportedExtension,
    ] {
        let connection = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
        connection.with_failure(kind);
        connection.with_call(McpRawCallResult {
            content: vec![McpRawContent::Text {
                text: "created".into(),
            }],
            structured_content: Some(json!({"id": 1})),
            is_error: false,
        });
        let registry = McpRegistry::new(FixtureFactory::new(vec![connection.clone()]));
        registry
            .refresh(
                candidate(server(1)),
                None,
                McpRuntimeConfig::test_default(),
                CancellationToken::new(),
            )
            .await
            .expect("refresh")
            .finish()
            .await;
        let key = McpServerKey::new("github").expect("key");
        let invocation = registry
            .resolve(&key, "create_issue", json!({}))
            .await
            .expect("resolve");
        let failure = registry
            .execute(
                &invocation,
                None,
                &UnavailableMcpImageMaterializer,
                CancellationToken::new(),
            )
            .await
            .expect_err("first request must fail");
        assert!(failure.remote_may_have_executed);
        assert!(!invocation.is_retired());
        assert_eq!(connection.closed.load(Ordering::SeqCst), 0);
        assert_eq!(
            connection.calls.lock().expect("calls").len(),
            1,
            "failed request must not replay"
        );
        assert!(registry.diagnostics().expect("diagnostics").is_empty());
        let next = registry
            .resolve(&key, "create_issue", json!({}))
            .await
            .expect("still discoverable");
        let result = registry
            .execute(
                &next,
                None,
                &UnavailableMcpImageMaterializer,
                CancellationToken::new(),
            )
            .await
            .expect("next explicit request succeeds");
        assert!(!result.is_error);
        registry.shutdown().await.expect("shutdown");
        assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn broken_connections_are_retired_without_replaying_request() {
    for kind in [
        McpConnectionFailureKind::Connect,
        McpConnectionFailureKind::Protocol,
    ] {
        let connection = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
        connection.with_failure(kind);
        let registry = McpRegistry::new(FixtureFactory::new(vec![connection.clone()]));
        registry
            .refresh(
                candidate(server(1)),
                None,
                McpRuntimeConfig::test_default(),
                CancellationToken::new(),
            )
            .await
            .expect("refresh")
            .finish()
            .await;
        let key = McpServerKey::new("github").expect("key");
        let invocation = registry
            .resolve(&key, "create_issue", json!({}))
            .await
            .expect("resolve");
        let failure = registry
            .execute(
                &invocation,
                None,
                &UnavailableMcpImageMaterializer,
                CancellationToken::new(),
            )
            .await
            .expect_err("broken connection");
        assert_eq!(failure.kind, McpCallFailureKind::RequestFailed);
        assert!(failure.remote_may_have_executed);
        assert!(invocation.is_retired());
        assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
        assert_eq!(registry.diagnostics().expect("diagnostics").len(), 1);
        assert_eq!(
            registry
                .resolve(&key, "create_issue", json!({}))
                .await
                .expect_err("unavailable")
                .kind,
            McpCallFailureKind::ServerUnavailable
        );
        registry.shutdown().await.expect("shutdown");
    }
}

#[tokio::test]
async fn conflicting_sources_retire_existing_connection_and_publish_diagnostic() {
    let old = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![old.clone()]));
    let key = McpServerKey::new("github").expect("key");
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("initial")
        .finish()
        .await;

    let mut conflicted = candidate(server(2));
    conflicted.servers.clear();
    conflicted.blocked_keys.insert(key.clone());
    conflicted.diagnostics.push(McpDiagnosticSnapshot {
        server_key: Some(key.clone()),
        code: McpDiagnosticCode::InvalidConfig,
        field_path: None,
        message: "plugin MCP server name conflicts with another source".to_owned(),
    });
    let result = registry
        .refresh(
            conflicted,
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("conflict")
        .finish()
        .await;
    assert_eq!(result.outcome, McpRefreshOutcome::Failure);
    assert_eq!(
        result.servers[0].outcome,
        McpServerRefreshOutcome::BlockedByConflict
    );
    assert!(registry.current(&key).expect("registry").is_none());
    assert_eq!(old.closed.load(Ordering::SeqCst), 1);
    assert_eq!(registry.diagnostics().expect("diagnostics").len(), 1);
}

#[tokio::test]
async fn independent_server_remains_available_when_plugin_name_conflicts() {
    let independent = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![independent.clone()]));
    let mut configured = candidate(server(1));
    let key = McpServerKey::new("github").expect("key");
    configured
        .supplemental_diagnostics
        .push(McpDiagnosticSnapshot {
            server_key: Some(key.clone()),
            code: McpDiagnosticCode::InvalidConfig,
            field_path: None,
            message: "plugin MCP server name conflicts with an independent server".to_owned(),
        });
    let result = registry
        .refresh(
            configured,
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    assert_eq!(result.outcome, McpRefreshOutcome::Success);
    assert_eq!(
        result.servers[0].outcome,
        McpServerRefreshOutcome::Refreshed
    );
    assert!(registry.current(&key).expect("registry").is_some());
    assert!(registry.diagnostics().unwrap().iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("plugin MCP server name conflicts")
    }));
    registry.shutdown().await.expect("shutdown");
    assert_eq!(independent.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retiring_plugin_package_closes_idle_connection_before_file_change() {
    let old = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![old.clone()]));
    let mut configured = server(1);
    configured.plugin_name = Some("example-tools".into());
    configured.plugin_sha256 = Some("a".repeat(64));
    let key = configured.server_key.clone();
    registry
        .refresh(
            candidate(configured),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("initial")
        .finish()
        .await;
    assert!(
        registry
            .retire_plugin_package("example-tools", &"b".repeat(64))
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(old.closed.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry
            .retire_plugin_package("example-tools", &"a".repeat(64))
            .await
            .unwrap(),
        (vec![key.clone()], false)
    );
    assert!(registry.current(&key).expect("registry").is_none());
    assert_eq!(old.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn server_tool_timeout_overrides_default_in_both_directions() {
    for timeout in [
        None,
        Some(Duration::from_millis(500)),
        Some(Duration::from_secs(300)),
        Some(Duration::from_secs(1800)),
    ] {
        let connection = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
        let registry = McpRegistry::new(FixtureFactory::new(vec![connection]));
        let runtime = McpRuntimeConfig::test_default();
        let mut configured = server(1);
        let key = configured.server_key.clone();
        configured.tool_timeout = timeout;
        registry
            .refresh(
                candidate(configured),
                None,
                runtime,
                CancellationToken::new(),
            )
            .await
            .expect("refresh")
            .finish()
            .await;
        // 核对实际执行持有的时限，防止配置已保存但 Registry 仍截断到全局默认。
        assert_eq!(
            registry
                .current(&key)
                .expect("registry")
                .expect("server")
                .request_timeout,
            timeout.unwrap_or(runtime.request_timeout())
        );
        registry.shutdown().await.expect("shutdown");
    }
}

#[tokio::test]
async fn replacement_rejects_old_approval_identity_before_old_calls_finish() {
    let old = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let next = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![old.clone(), next]));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("initial")
        .finish()
        .await;
    let key = McpServerKey::new("github").expect("key");
    let invocation = registry
        .resolve_identity(&key, "create_issue", Map::new())
        .expect("old invocation");
    let lease = registry
        .current(&key)
        .expect("current")
        .expect("server")
        .acquire_lease()
        .expect("in-flight call");
    // 相同配置与 Schema 也必须识别为新连接；提交不得等待旧调用，审批失效可先发生。
    let committed = tokio::time::timeout(
        Duration::from_secs(1),
        registry.refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("commit must not wait for lease")
    .expect("refresh");
    assert_eq!(old.closed.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry
            .validate_invocation(&invocation)
            .await
            .expect_err("stale approval")
            .kind,
        McpCallFailureKind::CatalogChanged
    );
    assert_eq!(
        registry
            .execute(
                &invocation,
                None,
                &UnavailableMcpImageMaterializer,
                CancellationToken::new()
            )
            .await
            .expect_err("must not call replacement")
            .kind,
        McpCallFailureKind::CatalogChanged
    );
    drop(lease);
    committed.finish().await;
    assert_eq!(old.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refresh_and_shutdown_report_connection_cleanup_failures() {
    let old = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let next = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    old.close_failed.store(true, Ordering::SeqCst);
    next.close_failed.store(true, Ordering::SeqCst);
    let registry = McpRegistry::new(FixtureFactory::new(vec![old, next]));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("initial")
        .finish()
        .await;
    let result = registry
        .refresh(
            candidate(server(2)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    assert_eq!(result.outcome, McpRefreshOutcome::Partial);
    assert_eq!(
        result.servers[0].outcome,
        McpServerRefreshOutcome::Refreshed
    );
    assert!(result.servers[0].diagnostic.is_some());
    assert!(registry.shutdown().await.is_err());
}

#[tokio::test]
async fn long_description_warnings_do_not_block_loading_or_calls_and_clear_on_refresh() {
    let mut long = tool(json!({"type":"object"}));
    let description = "design instructions ".repeat(1_000);
    long.description = Some(description.clone());
    let mut second = long.clone();
    second.name = "second_tool".to_owned();
    let connection = FixtureConnection::new(vec![long, second]);
    connection.with_call(McpRawCallResult {
        content: vec![McpRawContent::Text {
            text: "created".to_owned(),
        }],
        structured_content: Some(json!({"id":1})),
        is_error: false,
    });
    let short = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![connection.clone(), short]));
    let key = McpServerKey::new("github").expect("key");
    let refreshed = registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    assert_eq!(refreshed.outcome, McpRefreshOutcome::Success);
    assert_eq!(
        refreshed.servers[0].outcome,
        McpServerRefreshOutcome::Refreshed
    );
    assert_eq!(refreshed.servers[0].tool_count, 2);
    assert_eq!(
        refreshed.servers[0]
            .diagnostic
            .as_ref()
            .expect("warning")
            .code,
        McpDiagnosticCode::ToolDescriptionLong
    );
    assert_eq!(registry.diagnostics().expect("warnings").len(), 2);
    let catalog = registry
        .catalog_server(&key)
        .expect("catalog")
        .expect("server");
    assert_eq!(
        catalog.tools[0].description.as_deref(),
        Some(description.as_str())
    );
    let invocation = registry
        .resolve(&key, "create_issue", json!({}))
        .await
        .expect("resolve");
    let result = registry
        .execute(
            &invocation,
            None,
            &UnavailableMcpImageMaterializer,
            CancellationToken::new(),
        )
        .await
        .expect("call");
    assert!(!result.is_error);
    registry
        .refresh(
            candidate(server(2)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("replace")
        .finish()
        .await;
    assert!(
        registry
            .diagnostics()
            .expect("no stale warnings")
            .is_empty()
    );
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
    registry.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn refresh_compiles_schema_and_call_projects_ordered_content() {
    let connection = FixtureConnection::new(vec![tool(json!({
        "type": "object",
        "required": ["title"],
        "properties": {"title": {"type": "string"}}
    }))]);
    connection.with_call(McpRawCallResult {
        content: vec![
            McpRawContent::Text {
                text: "created".to_owned(),
            },
            McpRawContent::ResourceLink {
                uri: "https://example.com/issues/1".to_owned(),
                name: "issue-1".to_owned(),
                title: None,
                description: None,
                media_type: Some("text/html".to_owned()),
                size: None,
            },
        ],
        structured_content: Some(json!({"id": 1})),
        is_error: false,
    });
    let registry = McpRegistry::new(FixtureFactory::new(vec![connection.clone()]));
    let refresh = registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    assert_eq!(refresh.outcome, McpRefreshOutcome::Success);
    let projection = registry
        .projections()
        .expect("projection")
        .remove(&McpServerKey::new("github").expect("key"))
        .expect("server");
    assert_eq!(projection.state, McpServerRuntimeState::Connected);
    assert_eq!(projection.tool_count, 1);

    let invalid = registry
        .resolve(
            &McpServerKey::new("github").expect("key"),
            "create_issue",
            json!({"title": 1}),
        )
        .await
        .expect_err("invalid input");
    assert_eq!(invalid.kind, McpCallFailureKind::InvalidInput);
    assert!(!invalid.remote_may_have_executed);

    let invocation = registry
        .resolve(
            &McpServerKey::new("github").expect("key"),
            "create_issue",
            json!({"title": "bug"}),
        )
        .await
        .expect("resolve");
    assert_eq!(invocation.server_display_name, "GitHub");
    let result = registry
        .execute(
            &invocation,
            None,
            &UnavailableMcpImageMaterializer,
            CancellationToken::new(),
        )
        .await
        .expect("execute");
    assert!(!result.is_error);
    assert!(result.remote_may_have_executed);
    assert_eq!(result.content.as_parts().len(), 3);
    assert!(matches!(
        result.content.as_parts()[0],
        ToolResultPart::Text { .. }
    ));
    assert!(matches!(
        result.content.as_parts()[2],
        ToolResultPart::Json { .. }
    ));
    registry.shutdown().await.expect("shutdown");
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fixed_gateway_freezes_real_identity_and_preserves_remote_error_content() {
    let connection = FixtureConnection::new(vec![tool(json!({
        "type": "object",
        "required": ["title"],
        "properties": {"title": {"type": "string"}}
    }))]);
    connection.with_call(McpRawCallResult {
        content: vec![McpRawContent::Text {
            text: "remote rejected issue".to_owned(),
        }],
        structured_content: None,
        is_error: true,
    });
    let registry = Arc::new(McpRegistry::new(FixtureFactory::new(vec![connection])));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    let key = McpServerKey::new("github").expect("server");
    let mut tools = ToolRegistry::new();
    tools
        .register(crate::mcp::CallMcpTool::new(
            registry.clone(),
            crate::mcp::McpDisclosureScope::new(BTreeSet::from([key.clone()])),
            None,
            Arc::new(UnavailableMcpImageMaterializer),
        ))
        .expect("register gateway");
    let call = ToolCall {
        id: ToolCallId::new("call-mcp-1").expect("call id"),
        name: ToolName::new("call_mcp_tool").expect("tool name"),
        arguments: json!({
            "server": "github",
            "tool": "create_issue",
            "arguments": {"title": "bug"}
        }),
    };
    let mut batch = Dispatcher::resolve_batch(&tools.snapshot(), &[call]);
    let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("item") else {
        panic!("gateway resolves");
    };
    let facts = invocation
        .facts::<crate::mcp::McpAuthorizationFacts>()
        .expect("MCP facts");
    assert_eq!(facts.invocation.server_key, key);
    assert_eq!(facts.invocation.tool_name, "create_issue");
    assert_eq!(
        facts.invocation.untrusted_annotations,
        Some(json!({"destructiveHint": true}))
    );
    registry
        .validate_invocation(&facts.invocation)
        .await
        .expect("arguments validate before authorization");

    let result = Dispatcher::execute(&mut batch, 0, ToolContext::default())
        .expect("dispatch")
        .await;
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(
        result.content.as_single_text(),
        Some("remote rejected issue")
    );
}

#[tokio::test]
async fn failed_candidate_keeps_old_state_and_closes_only_failed_connection() {
    let old = FixtureConnection::new(vec![tool(json!({"type": "object"}))]);
    let failed = FixtureConnection::new(vec![tool(json!({
        "type": "object",
        "properties": {"x": {"$ref": "https://example.com/schema"}}
    }))]);
    let registry = McpRegistry::new(FixtureFactory::new(vec![old.clone(), failed.clone()]));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("first refresh")
        .finish()
        .await;
    let result = registry
        .refresh(
            candidate(server(2)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("failed refresh")
        .finish()
        .await;
    assert_eq!(result.outcome, McpRefreshOutcome::Failure);
    assert_eq!(
        result.servers[0].outcome,
        McpServerRefreshOutcome::RetainedAfterFailure
    );
    assert_eq!(
        registry
            .projections()
            .expect("projection")
            .values()
            .next()
            .expect("server")
            .fingerprint,
        [1; 32]
    );
    assert_eq!(failed.closed.load(Ordering::SeqCst), 1);
    assert_eq!(old.closed.load(Ordering::SeqCst), 0);
    registry.shutdown().await.expect("shutdown");
    assert_eq!(old.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retired_connection_waits_for_existing_lease_before_close() {
    let connection = FixtureConnection::new(vec![tool(json!({"type": "object"}))]);
    let registry = Arc::new(McpRegistry::new(FixtureFactory::new(vec![
        connection.clone(),
    ])));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    let state = registry
        .servers
        .read()
        .expect("servers")
        .values()
        .next()
        .expect("server")
        .clone();
    let lease = state.acquire_lease().expect("lease");
    let removed = registry
        .remove_current(&state.server_key)
        .expect("remove")
        .expect("state");
    let retire = tokio::spawn(async move { removed.retire().await });
    tokio::task::yield_now().await;
    assert_eq!(connection.closed.load(Ordering::SeqCst), 0);
    drop(lease);
    retire.await.expect("retire task").expect("retire");
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsupported_content_reports_that_remote_may_have_executed() {
    let connection = FixtureConnection::new(vec![tool(json!({"type": "object"}))]);
    connection.with_call(McpRawCallResult {
        content: vec![McpRawContent::Audio],
        structured_content: None,
        is_error: false,
    });
    let registry = McpRegistry::new(FixtureFactory::new(vec![connection]));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .expect("refresh")
        .finish()
        .await;
    let invocation = registry
        .resolve(
            &McpServerKey::new("github").expect("key"),
            "create_issue",
            json!({}),
        )
        .await
        .expect("resolve");
    let failure = registry
        .execute(
            &invocation,
            None,
            &UnavailableMcpImageMaterializer,
            CancellationToken::new(),
        )
        .await
        .expect_err("unsupported");
    assert_eq!(failure.kind, McpCallFailureKind::UnsupportedResult);
    assert!(failure.remote_may_have_executed);
}

#[tokio::test]
async fn plugin_retirement_cancellation_interrupts_call_without_cancelling_run() {
    let connection = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    connection.block_call.store(true, Ordering::SeqCst);
    let registry = Arc::new(McpRegistry::new(FixtureFactory::new(vec![
        connection.clone(),
    ])));
    registry
        .refresh(
            candidate(server(1)),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .finish()
        .await;
    let key = McpServerKey::new("github").unwrap();
    let invocation = registry
        .resolve(&key, "create_issue", json!({}))
        .await
        .unwrap();
    let state = registry.current(&key).unwrap().unwrap();
    let run_cancellation = CancellationToken::new();
    let run_token = run_cancellation.clone();
    let executing = registry.clone();
    let task = tokio::spawn(async move {
        executing
            .execute(
                &invocation,
                None,
                &UnavailableMcpImageMaterializer,
                run_token,
            )
            .await
    });
    connection.call_started.notified().await;
    state.call_cancellation.cancel();
    let removed = registry.remove_current(&key).unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(2), removed.retire())
        .await
        .unwrap()
        .unwrap();
    let failure = task.await.unwrap().unwrap_err();
    assert_eq!(failure.kind, McpCallFailureKind::Cancelled);
    assert!(failure.remote_may_have_executed);
    assert!(!run_cancellation.is_cancelled());
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
    assert_eq!(state.active_leases.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn plugin_retirement_reports_close_failure() {
    let connection = FixtureConnection::new(vec![tool(json!({"type":"object"}))]);
    connection.close_failed.store(true, Ordering::SeqCst);
    let registry = McpRegistry::new(FixtureFactory::new(vec![connection.clone()]));
    let mut configured = server(1);
    configured.plugin_name = Some("example-tools".into());
    configured.plugin_sha256 = Some("a".repeat(64));
    registry
        .refresh(
            candidate(configured),
            None,
            McpRuntimeConfig::test_default(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .finish()
        .await;
    assert!(
        registry
            .retire_plugin_package("example-tools", &"a".repeat(64))
            .await
            .unwrap()
            .1
    );
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
}
