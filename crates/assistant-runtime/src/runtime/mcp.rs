//! MCP 管理命令与临时候选连接测试。

use std::time::Instant;

use assistant_protocol::{
    GetMcpConfigurationRequest, GetMcpConfigurationResult, ListMcpServerOptionsRequest,
    ListMcpServerOptionsResult, McpConnectionTestOutcome, McpConnectionTestStage,
    McpDiagnosticCode, McpDiagnosticSnapshot, McpRefreshControlResultSnapshot,
    McpServerOptionSnapshot, McpServerOptionsContext, MutateMcpConfigurationRequest,
    MutateMcpConfigurationResult, PreviewMcpImportRequest, PreviewMcpImportResult,
    TestMcpServerRequest, TestMcpServerResult,
};

use super::AssistantRuntime;
use crate::{
    McpConnectionFailureKind, McpConnectionOptions, RuntimeError, RuntimeResult,
    mcp::effective_connect_timeout,
};

impl AssistantRuntime {
    pub fn list_mcp_server_options(
        &self,
        request: ListMcpServerOptionsRequest,
    ) -> RuntimeResult<ListMcpServerOptionsResult> {
        self.ensure_running()?;
        let scopes = match request.context {
            McpServerOptionsContext::Session { session_id } => {
                let session = self.session(&session_id)?;
                session.ensure_active()?;
                session.permission_scopes()
            }
            McpServerOptionsContext::NewSession { workspace_id } => {
                let mut scopes = vec![crate::PermissionFileScope::Global];
                if let Some(workspace_id) = workspace_id {
                    self.workspace_for_new_session(&workspace_id)?;
                    scopes.push(crate::PermissionFileScope::Workspace(workspace_id));
                }
                scopes
            }
        };
        let mut servers = Vec::new();
        for server in self.mcp_service.registry.catalog_snapshot()? {
            let mut visible_tool_count = 0_u32;
            for tool in &server.tools {
                if !self.permission_coordinator.mcp_tool_is_explicitly_denied(
                    &scopes,
                    request.variant,
                    &server.server_key,
                    &tool.name,
                )? {
                    visible_tool_count = visible_tool_count.saturating_add(1);
                }
            }
            if visible_tool_count > 0 {
                servers.push(McpServerOptionSnapshot {
                    server_key: server.server_key,
                    display_name: server.display_name,
                    description: server.description,
                    visible_tool_count,
                });
            }
        }
        Ok(ListMcpServerOptionsResult { servers })
    }

    pub async fn get_mcp_configuration(
        &self,
        _request: GetMcpConfigurationRequest,
    ) -> RuntimeResult<GetMcpConfigurationResult> {
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        let runtime = self.mcp_service.registry.projections()?;
        let mut snapshot = self
            .mcp_service
            .config_store
            .snapshot_with_runtime(&runtime)
            .await?;
        merge_registry_diagnostics(
            &mut snapshot.diagnostics,
            self.mcp_service.registry.diagnostics()?,
        );
        Ok(GetMcpConfigurationResult { snapshot })
    }

    pub async fn preview_mcp_import(
        &self,
        request: PreviewMcpImportRequest,
    ) -> RuntimeResult<PreviewMcpImportResult> {
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        self.mcp_service
            .config_store
            .preview_import(&request.document)
            .await
    }

    pub async fn mutate_mcp_configuration(
        &self,
        request: MutateMcpConfigurationRequest,
    ) -> RuntimeResult<MutateMcpConfigurationResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        let runtime = self.mcp_service.registry.projections()?;
        let mut snapshot = self
            .mcp_service
            .config_store
            .mutate_with_runtime(&request.expected_revision, request.mutation, &runtime)
            .await?;
        merge_registry_diagnostics(
            &mut snapshot.diagnostics,
            self.mcp_service.registry.diagnostics()?,
        );
        Ok(MutateMcpConfigurationResult { snapshot })
    }

    /// 在 Host 发布监听端点前，从最新 `mcp.json` 建立一次无 Conversation 副作用的活动目录。
    pub async fn bootstrap_mcp(&self) -> RuntimeResult<McpRefreshControlResultSnapshot> {
        self.refresh_mcp_registry(None).await
    }

    pub(crate) async fn refresh_mcp_registry(
        &self,
        target: Option<&assistant_protocol::McpServerKey>,
    ) -> RuntimeResult<McpRefreshControlResultSnapshot> {
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        refresh_mcp_registry_with(
            self.mcp_service.config_store.as_ref(),
            self.mcp_service.registry.as_ref(),
            self.config_registry.as_ref(),
            self.approval_registry.as_ref(),
            &self.event_sender,
            &self.root_cancellation,
            target,
        )
        .await
    }

    pub async fn test_mcp_server(
        &self,
        request: TestMcpServerRequest,
    ) -> RuntimeResult<TestMcpServerResult> {
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        let cancellation = self.mcp_test_cancellation(&request.test_id)?;
        let _cancel_on_exit = cancellation.clone().drop_guard();
        let result = self
            .run_mcp_server_test(&request, cancellation.clone())
            .await;
        // 短时取消标记覆盖“关闭页面”先于测试请求到达的竞态，且不留下持久化状态。
        cancellation.cancel();
        result
    }

    pub fn cancel_mcp_server_test(
        &self,
        request: assistant_protocol::CancelMcpServerTestRequest,
    ) -> RuntimeResult<assistant_protocol::CancelMcpServerTestResult> {
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        self.mcp_test_cancellation(&request.test_id)?.cancel();
        Ok(assistant_protocol::CancelMcpServerTestResult::default())
    }

    fn mcp_test_cancellation(
        &self,
        test_id: &assistant_protocol::IdempotencyKey,
    ) -> RuntimeResult<tokio_util::sync::CancellationToken> {
        let mut tests =
            self.mcp_service
                .tests
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "MCP connection tests",
                })?;
        tests.retain(|_, (created, token)| {
            !token.is_cancelled() || created.elapsed().as_secs() < 60
        });
        if let Some((_, token)) = tests.get(test_id) {
            return Ok(token.clone());
        }
        if tests.len() >= 64 {
            return Err(RuntimeError::InvalidRequest {
                reason: "too many MCP connection tests; retry later",
            });
        }
        let token = self.root_cancellation.child_token();
        tests.insert(test_id.clone(), (Instant::now(), token.clone()));
        Ok(token)
    }

    async fn run_mcp_server_test(
        &self,
        request: &TestMcpServerRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> RuntimeResult<TestMcpServerResult> {
        let started = Instant::now();
        let _test = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(cancelled_test(started)),
            guard = self.mcp_service.test_gate.lock() => guard,
        };
        let server_key = request.server.server_key.clone();
        let server = self
            .mcp_service
            .config_store
            .resolve_draft(&request.server)
            .await?;
        let snapshot = self.config_registry.snapshot()?;
        let runtime_config = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?
            .mcp();
        if cancellation.is_cancelled() {
            return Ok(cancelled_test(started));
        }
        let connect = self.mcp_service.connection_factory.connect(
            server.clone(),
            McpConnectionOptions {
                max_concurrent_requests: usize::try_from(
                    runtime_config.max_concurrent_calls_per_server().get(),
                )
                .unwrap_or(1),
                control_timeout: runtime_config.close_timeout(),
            },
            cancellation.clone(),
        );
        let connect_result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(cancelled_test(started)),
            result = tokio::time::timeout(effective_connect_timeout(runtime_config, &server), connect) => result,
        };
        let connection = match connect_result {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                return Ok(connection_failure(server_key, &error, started));
            }
            Err(_) => {
                cancellation.cancel();
                return Ok(test_failure(
                    server_key,
                    McpConnectionTestStage::Connect,
                    McpDiagnosticCode::ConnectFailed,
                    "MCP connection timed out",
                    started,
                ));
            }
        };

        let catalog = tokio::time::timeout(
            runtime_config.catalog_timeout(),
            self.mcp_service.registry.build_catalog(
                &server_key,
                connection.clone(),
                cancellation.clone(),
            ),
        );
        let catalog_result = match tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = catalog => Some(result),
        } {
            None => Err(cancelled_test(started)),
            Some(Ok(Ok(tools))) => Ok(tools),
            Some(Ok(Err(diagnostic))) => Err(TestMcpServerResult {
                outcome: McpConnectionTestOutcome::Failure,
                stage: McpConnectionTestStage::Catalog,
                elapsed_ms: elapsed_ms(started),
                tool_count: 0,
                diagnostic: Some(diagnostic),
            }),
            Some(Err(_)) => {
                cancellation.cancel();
                Err(test_failure(
                    server_key.clone(),
                    McpConnectionTestStage::Catalog,
                    McpDiagnosticCode::CatalogFailed,
                    "MCP catalog request timed out",
                    started,
                ))
            }
        };

        let close_result = tokio::time::timeout(
            runtime_config.close_timeout(),
            connection.close(self.root_cancellation.child_token()),
        )
        .await;
        let tools = match catalog_result {
            Ok(tools) => tools,
            Err(failure) => return Ok(failure),
        };
        if cancellation.is_cancelled() {
            return Ok(cancelled_test(started));
        }
        match close_result {
            Ok(Ok(())) => Ok(TestMcpServerResult {
                outcome: McpConnectionTestOutcome::Success,
                stage: McpConnectionTestStage::Complete,
                elapsed_ms: elapsed_ms(started),
                tool_count: u32::try_from(tools.len()).unwrap_or(u32::MAX),
                diagnostic: tools
                    .values()
                    .find_map(|tool| tool.description_warning(&server_key)),
            }),
            Ok(Err(error)) => Ok(test_failure(
                server_key,
                McpConnectionTestStage::Close,
                code_for_failure(error.kind()),
                error.message(),
                started,
            )),
            Err(_) => {
                cancellation.cancel();
                Ok(test_failure(
                    server_key,
                    McpConnectionTestStage::Close,
                    McpDiagnosticCode::ProtocolFailed,
                    "MCP connection close timed out",
                    started,
                ))
            }
        }
    }
}

fn cancelled_test(started: Instant) -> TestMcpServerResult {
    TestMcpServerResult {
        outcome: McpConnectionTestOutcome::Cancelled,
        stage: McpConnectionTestStage::Close,
        elapsed_ms: elapsed_ms(started),
        tool_count: 0,
        diagnostic: None,
    }
}

pub(super) async fn refresh_mcp_registry_with(
    config_store: &crate::mcp::McpConfigStore,
    registry: &crate::mcp::McpRegistry,
    config_registry: &crate::config::ConfigRegistry,
    approval_registry: &crate::permission::ApprovalRegistry,
    events: &crate::observation::ObservationCoordinator,
    root_cancellation: &tokio_util::sync::CancellationToken,
    target: Option<&assistant_protocol::McpServerKey>,
) -> RuntimeResult<McpRefreshControlResultSnapshot> {
    let candidate = config_store.registry_candidate().await?;
    let snapshot = config_registry.snapshot()?;
    let runtime_config = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?
        .mcp();
    let result = registry
        .refresh(
            candidate,
            target,
            runtime_config,
            root_cancellation.child_token(),
        )
        .await?;
    let mut invalidation_error = None;
    for server in &result.result.servers {
        if matches!(
            server.outcome,
            assistant_protocol::McpServerRefreshOutcome::Refreshed
                | assistant_protocol::McpServerRefreshOutcome::ConnectedWithoutTools
                | assistant_protocol::McpServerRefreshOutcome::Removed
                | assistant_protocol::McpServerRefreshOutcome::Disabled
        ) {
            let approvals =
                match approval_registry.invalidate_pending_mcp_server(&server.server_key) {
                    Ok(approvals) => approvals,
                    Err(error) => {
                        invalidation_error = Some(error);
                        continue;
                    }
                };
            for approval in approvals {
                let _ = events.send(assistant_protocol::RuntimeEvent::ApprovalCancelled {
                    session_id: approval.session_id,
                    run_id: approval.run_id,
                    child_task_id: approval.child_task_id,
                    approval_id: approval.approval_id,
                });
            }
        }
    }
    // 发布已经完成；即使审批状态写入失败，也必须完成旧连接的显式回收。
    let result = result.finish().await;
    if let Some(error) = invalidation_error {
        return Err(error);
    }
    Ok(result)
}

fn test_failure(
    server_key: assistant_protocol::McpServerKey,
    stage: McpConnectionTestStage,
    code: McpDiagnosticCode,
    message: &'static str,
    started: Instant,
) -> TestMcpServerResult {
    TestMcpServerResult {
        outcome: McpConnectionTestOutcome::Failure,
        stage,
        elapsed_ms: elapsed_ms(started),
        tool_count: 0,
        diagnostic: Some(McpDiagnosticSnapshot {
            server_key: Some(server_key),
            code,
            field_path: None,
            message: message.to_owned(),
        }),
    }
}

fn connection_failure(
    server_key: assistant_protocol::McpServerKey,
    error: &crate::McpConnectionError,
    started: Instant,
) -> TestMcpServerResult {
    let mut result = test_failure(
        server_key,
        stage_for_failure(error.kind()),
        code_for_failure(error.kind()),
        error.message(),
        started,
    );
    if error.kind() == McpConnectionFailureKind::Cancelled {
        result.outcome = McpConnectionTestOutcome::Cancelled;
    }
    result
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn stage_for_failure(kind: McpConnectionFailureKind) -> McpConnectionTestStage {
    match kind {
        McpConnectionFailureKind::InvalidConfig | McpConnectionFailureKind::Connect => {
            McpConnectionTestStage::Connect
        }
        McpConnectionFailureKind::Protocol | McpConnectionFailureKind::UnsupportedExtension => {
            McpConnectionTestStage::Protocol
        }
        McpConnectionFailureKind::Catalog | McpConnectionFailureKind::ToolCall => {
            McpConnectionTestStage::Catalog
        }
        McpConnectionFailureKind::Cancelled => McpConnectionTestStage::Connect,
        McpConnectionFailureKind::Close => McpConnectionTestStage::Close,
    }
}

fn code_for_failure(kind: McpConnectionFailureKind) -> McpDiagnosticCode {
    match kind {
        McpConnectionFailureKind::InvalidConfig => McpDiagnosticCode::InvalidConfig,
        McpConnectionFailureKind::Connect | McpConnectionFailureKind::Cancelled => {
            McpDiagnosticCode::ConnectFailed
        }
        McpConnectionFailureKind::Protocol
        | McpConnectionFailureKind::UnsupportedExtension
        | McpConnectionFailureKind::Close => McpDiagnosticCode::ProtocolFailed,
        McpConnectionFailureKind::Catalog | McpConnectionFailureKind::ToolCall => {
            McpDiagnosticCode::CatalogFailed
        }
    }
}

fn merge_registry_diagnostics(
    configuration: &mut Vec<McpDiagnosticSnapshot>,
    runtime: Vec<McpDiagnosticSnapshot>,
) {
    for diagnostic in runtime {
        if !configuration.iter().any(|current| {
            current.server_key == diagnostic.server_key
                && current.code == diagnostic.code
                && current.field_path == diagnostic.field_path
        }) {
            configuration.push(diagnostic);
        }
    }
    configuration.sort_by(|left, right| {
        left.server_key
            .cmp(&right.server_key)
            .then_with(|| format!("{:?}", left.code).cmp(&format!("{:?}", right.code)))
            .then_with(|| left.field_path.cmp(&right.field_path))
    });
}
