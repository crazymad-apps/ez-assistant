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
    pub async fn list_mcp_server_options(
        &self,
        request: ListMcpServerOptionsRequest,
    ) -> RuntimeResult<ListMcpServerOptionsResult> {
        self.ensure_running()?;
        let scopes = match request.context {
            McpServerOptionsContext::Session { session_id } => {
                let session = self.session(&session_id).await?;
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
                    plugin_name: server.plugin_name,
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
        request: GetMcpConfigurationRequest,
    ) -> RuntimeResult<GetMcpConfigurationResult> {
        self.ensure_running()?;
        self.mcp_management().get_mcp_configuration(request).await
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
        self.ensure_running()?;
        self.mcp_management()
            .mutate_mcp_configuration(request)
            .await
    }

    /// 配置页和 Agent 共用的单目标刷新；不创建输入、Run 或 Conversation。
    pub async fn refresh_mcp_server(
        &self,
        request: assistant_protocol::RefreshMcpServerRequest,
    ) -> RuntimeResult<McpRefreshControlResultSnapshot> {
        self.ensure_running()?;
        self.mcp_management()
            .refresh_mcp_server(request, self.root_cancellation.child_token())
            .await
    }

    /// 从最新 `mcp.json` 建立一次无 Conversation 副作用的活动目录；不阻塞 Host 监听。
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
            self.plugin.as_ref(),
            &self.mcp_service.builtin_definitions,
            self.mcp_service.registry.as_ref(),
            self.config_registry.as_ref(),
            self.approval_registry.as_ref(),
            &self.event_sender,
            &self.root_cancellation,
            target,
            None,
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
        let (server_key, server) = match (&request.server, &request.managed) {
            (Some(draft), None) => (
                draft.server_key.clone(),
                self.mcp_service.config_store.resolve_draft(draft).await?,
            ),
            (None, Some(draft)) => {
                let discovery =
                    discover_mcp(&self.plugin, &self.mcp_service.builtin_definitions).await?;
                let definition = find_definition(
                    &discovery.definitions,
                    &draft.target,
                    &draft.expected_definition_digest,
                )?;
                let _lease = definition_lease(self.plugin.as_ref(), definition).await?;
                let fresh =
                    discover_mcp(&self.plugin, &self.mcp_service.builtin_definitions).await?;
                let definition = find_definition(
                    &fresh.definitions,
                    &draft.target,
                    &draft.expected_definition_digest,
                )?;
                if !definition.source_enabled {
                    return Err(RuntimeError::PluginPackageUnavailable);
                }
                let server = self
                    .mcp_service
                    .config_store
                    .managed_test_config(draft, definition)
                    .await?;
                (server.server_key().clone(), server)
            }
            _ => {
                return Err(RuntimeError::InvalidRequest {
                    reason: "exactly one MCP test source is required",
                });
            }
        };
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
                    "MCP 连接超时，请检查服务启动状态和连接配置",
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
        if let Some(draft) = &request.managed {
            // 测试候选已关闭；不能把旧文件／参数的成功当作当前草稿已验证。
            let fresh = discover_mcp(&self.plugin, &self.mcp_service.builtin_definitions).await?;
            let definition = find_definition(
                &fresh.definitions,
                &draft.target,
                &draft.expected_definition_digest,
            )?;
            let _lease = definition_lease(self.plugin.as_ref(), definition).await?;
            let fresh = discover_mcp(&self.plugin, &self.mcp_service.builtin_definitions).await?;
            let definition = find_definition(
                &fresh.definitions,
                &draft.target,
                &draft.expected_definition_digest,
            )?;
            let current = self
                .mcp_service
                .config_store
                .managed_test_config(draft, definition)
                .await?;
            if current.fingerprint() != server.fingerprint() {
                return Err(RuntimeError::McpConfigConflict);
            }
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

#[expect(
    clippy::too_many_arguments,
    reason = "shared refresh needs the existing Runtime owners without duplicating state"
)]
pub(super) async fn refresh_mcp_registry_with(
    config_store: &crate::mcp::McpConfigStore,
    plugin: &crate::plugin::PluginManager,
    builtin_definitions: &[crate::ManagedMcpServerDefinition],
    registry: &crate::mcp::McpRegistry,
    config_registry: &crate::config::ConfigRegistry,
    approval_registry: &crate::permission::ApprovalRegistry,
    events: &crate::observation::ObservationCoordinator,
    root_cancellation: &tokio_util::sync::CancellationToken,
    target: Option<&assistant_protocol::McpServerKey>,
    expected: Option<&assistant_protocol::RefreshMcpServerRequest>,
) -> RuntimeResult<McpRefreshControlResultSnapshot> {
    let discovery = discover_mcp(plugin, builtin_definitions).await?;
    let (frozen, mut candidate) = config_store
        .resolve_managed(
            &discovery.definitions,
            &discovery.configuration_sources,
            &registry.projections()?,
        )
        .await?;
    candidate
        .supplemental_diagnostics
        .extend(discovery.diagnostics);
    if let Some(expected) = expected {
        check_refresh_identity(&frozen, expected)?;
    }
    let snapshot = config_registry.snapshot()?;
    let runtime_config = snapshot
        .active()
        .ok_or(RuntimeError::ConfigurationUnavailable)?
        .mcp();
    let result = registry
        .refresh_checked(
            candidate,
            target,
            runtime_config,
            root_cancellation.child_token(),
            || async {
                // Registry 写门已拥有刷新；来源门仅在候选准备结束后取得，失败即关闭候选。
                // try_write 避免插件更新已持包门、等待 Registry 时形成锁环。
                let mut leases = Vec::new();
                let mut names = std::collections::BTreeSet::new();
                for definition in &discovery.definitions {
                    if let assistant_protocol::McpServerSource::Plugin(name) = &definition.source
                        && definition.source_available
                        && names.insert(name.clone())
                        && let Some(lease) = definition_lease(plugin, definition).await?
                    {
                        leases.push(lease);
                    }
                }
                let file_gate = config_store.gate.clone().lock_owned().await;
                let fresh = discover_mcp(plugin, builtin_definitions).await?;
                let (current, _) = config_store
                    .resolve_managed_locked(
                        &fresh.definitions,
                        &fresh.configuration_sources,
                        &registry.projections()?,
                    )
                    .await?;
                if configuration_stamp(&frozen) != configuration_stamp(&current) {
                    return Err(RuntimeError::McpConfigConflict);
                }
                Ok((leases, file_gate))
            },
        )
        .await?;
    let invalidation_error = events.publish_batch_transition(|| {
        let mut invalidation_error = None;
        let mut notifications = Vec::new();
        for server in &result.result.servers {
            if matches!(
                server.outcome,
                assistant_protocol::McpServerRefreshOutcome::Refreshed
                    | assistant_protocol::McpServerRefreshOutcome::ConnectedWithoutTools
                    | assistant_protocol::McpServerRefreshOutcome::Removed
                    | assistant_protocol::McpServerRefreshOutcome::Disabled
                    | assistant_protocol::McpServerRefreshOutcome::BlockedByConflict
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
                    notifications.push(assistant_protocol::RuntimeEvent::ApprovalCancelled {
                        session_id: approval.session_id,
                        run_id: approval.run_id,
                        child_task_id: approval.child_task_id,
                        approval_id: approval.approval_id,
                    });
                }
            }
        }
        Ok((invalidation_error, notifications))
    })?;
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
    message: &str,
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

/// 来源摘要只保护定义换代，插件参数与文件修订在各自实际来源复核。
fn find_definition<'a>(
    definitions: &'a [crate::ManagedMcpServerDefinition],
    target: &assistant_protocol::McpServerTarget,
    digest: &str,
) -> RuntimeResult<&'a crate::ManagedMcpServerDefinition> {
    let definition = definitions
        .iter()
        .find(|definition| {
            definition.source == target.source && definition.server_key == target.server_key
        })
        .ok_or(RuntimeError::McpServerUnavailable)?;
    if definition.definition_digest != digest {
        return Err(RuntimeError::McpConfigConflict);
    }
    Ok(definition)
}

fn managed_mutation_target(
    mutation: &assistant_protocol::McpConfigurationMutation,
) -> Option<(&assistant_protocol::McpServerTarget, &str)> {
    match mutation {
        assistant_protocol::McpConfigurationMutation::PatchManaged {
            target,
            expected_definition_digest,
            ..
        }
        | assistant_protocol::McpConfigurationMutation::ResetManaged {
            target,
            expected_definition_digest,
            ..
        } => Some((target, expected_definition_digest)),
        _ => None,
    }
}

async fn definition_lease(
    plugin: &crate::plugin::PluginManager,
    definition: &crate::ManagedMcpServerDefinition,
) -> RuntimeResult<Option<crate::PluginConfigLease>> {
    match &definition.source {
        assistant_protocol::McpServerSource::Plugin(name) => Ok(Some(
            plugin
                .config_lease(
                    name,
                    definition
                        .plugin_sha256
                        .as_deref()
                        .ok_or(RuntimeError::PluginPackageUnavailable)?,
                )
                .await?,
        )),
        assistant_protocol::McpServerSource::Builtin => Ok(None),
        assistant_protocol::McpServerSource::User => Err(RuntimeError::McpConfigInvalid),
    }
}

/// 不比较活动连接易失状态，避免一次无关调用故障让来源校验误报冲突。
fn configuration_stamp(snapshot: &assistant_protocol::McpConfigurationSnapshot) -> String {
    serde_json::to_string(&(
        snapshot.revision.as_str(),
        snapshot
            .servers
            .iter()
            .map(|server| {
                (
                    &server.source,
                    &server.server_key,
                    server.enabled,
                    &server.managed,
                )
            })
            .collect::<Vec<_>>(),
    ))
    .expect("safe MCP configuration identity serializes")
}

fn check_refresh_identity(
    snapshot: &assistant_protocol::McpConfigurationSnapshot,
    request: &assistant_protocol::RefreshMcpServerRequest,
) -> RuntimeResult<()> {
    let server = snapshot
        .servers
        .iter()
        .find(|server| {
            server.source == request.target.source && server.server_key == request.target.server_key
        })
        .ok_or(RuntimeError::McpServerUnavailable)?;
    let (revision, digest) =
        server
            .managed
            .as_ref()
            .map_or((snapshot.revision.as_str(), ""), |managed| {
                (
                    managed.revision.as_str(),
                    managed.definition_digest.as_str(),
                )
            });
    if revision != request.expected_revision || digest != request.expected_definition_digest {
        return Err(RuntimeError::McpConfigConflict);
    }
    Ok(())
}

/// 请求断开不丢失已经进入提交的文件写入及包租约。先在调用方等待文件门，
/// 再由现有 RuntimeTasks 拥有有限提交任务；没有无主写任务或额外配置队列。
async fn commit_managed_configuration(
    tasks: &super::tasks::RuntimeTasks,
    store: std::sync::Arc<crate::mcp::McpConfigStore>,
    definition: crate::ManagedMcpServerDefinition,
    request: MutateMcpConfigurationRequest,
    lease: Option<crate::PluginConfigLease>,
    events: crate::observation::ObservationCoordinator,
) -> RuntimeResult<()> {
    let file_gate = store.gate.clone().lock_owned().await;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tasks.spawn(
        async move {
            let _source = lease;
            let _file = file_gate;
            let result = store
                .mutate_managed_locked(&request.expected_revision, &request.mutation, &definition)
                .await;
            if result.is_ok() {
                let _ = events.send(assistant_protocol::RuntimeEvent::McpRegistryChanged);
            }
            let _ = sender.send(result);
        },
        async {},
    );
    receiver
        .await
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "MCP configuration commit",
        })?
}

#[cfg(test)]
#[path = "mcp/tests.rs"]
mod tests;

/// 当前用户域的 MCP 管理句柄，仅共享既有 owner，不持有 AssistantRuntime、Session 集合或第二份配置。
/// UI 命令与父 Run 工具共用这些方法；operation gate 保护提交准入，文件／包门不跨网络等待。
#[derive(Clone)]
pub(super) struct McpManagement {
    config_store: std::sync::Arc<crate::mcp::McpConfigStore>,
    registry: std::sync::Arc<crate::mcp::McpRegistry>,
    plugin: std::sync::Arc<crate::plugin::PluginManager>,
    config_registry: std::sync::Arc<crate::config::ConfigRegistry>,
    approval_registry: std::sync::Arc<crate::permission::ApprovalRegistry>,
    event_sender: crate::observation::ObservationCoordinator,
    root_cancellation: tokio_util::sync::CancellationToken,
    tasks: std::sync::Arc<super::tasks::RuntimeTasks>,
    operation_gate: std::sync::Arc<tokio::sync::RwLock<()>>,
    available: bool,
    builtin_definitions: std::sync::Arc<[crate::ManagedMcpServerDefinition]>,
}

impl AssistantRuntime {
    pub(super) fn mcp_management(&self) -> McpManagement {
        McpManagement {
            config_store: self.mcp_service.config_store.clone(),
            registry: self.mcp_service.registry.clone(),
            plugin: self.plugin.clone(),
            config_registry: self.config_registry.clone(),
            approval_registry: self.approval_registry.clone(),
            event_sender: self.event_sender.clone(),
            root_cancellation: self.root_cancellation.clone(),
            tasks: self.tasks.clone(),
            operation_gate: self.operation_gate.clone(),
            available: self.mcp_service.management_available(),
            builtin_definitions: self.mcp_service.builtin_definitions.clone(),
        }
    }
}

impl McpManagement {
    pub(super) fn builtin_definitions(&self) -> &[crate::ManagedMcpServerDefinition] {
        &self.builtin_definitions
    }

    pub(super) fn available(&self) -> bool {
        self.available
    }

    fn ensure_available(&self) -> RuntimeResult<()> {
        if self.root_cancellation.is_cancelled() {
            return Err(RuntimeError::RuntimeNotRunning {
                lifecycle: assistant_protocol::RuntimeLifecycle::ShuttingDown,
            });
        }
        if !self.available {
            return Err(RuntimeError::McpServerUnavailable);
        }
        Ok(())
    }
    pub(super) async fn get_mcp_configuration(
        &self,
        _request: GetMcpConfigurationRequest,
    ) -> RuntimeResult<GetMcpConfigurationResult> {
        self.ensure_available()?;

        let runtime = self.registry.projections()?;
        let definitions = discover_mcp(&self.plugin, &self.builtin_definitions).await?;
        let (mut snapshot, _) = self
            .config_store
            .resolve_managed(
                &definitions.definitions,
                &definitions.configuration_sources,
                &runtime,
            )
            .await?;
        merge_registry_diagnostics(&mut snapshot.diagnostics, definitions.diagnostics);
        merge_registry_diagnostics(&mut snapshot.diagnostics, self.registry.diagnostics()?);
        Ok(GetMcpConfigurationResult { snapshot })
    }

    pub(super) async fn mutate_mcp_configuration(
        &self,
        request: MutateMcpConfigurationRequest,
    ) -> RuntimeResult<MutateMcpConfigurationResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_available()?;

        if let Some((target, digest)) = managed_mutation_target(&request.mutation) {
            let discovery = discover_mcp(&self.plugin, &self.builtin_definitions).await?;
            let definition = find_definition(&discovery.definitions, target, digest)?;
            // 直接文件修改持包配置租约；参数修改另走原插件命令，禁止读租约升级。
            let _lease = definition_lease(self.plugin.as_ref(), definition).await?;
            let fresh = discover_mcp(&self.plugin, &self.builtin_definitions).await?;
            let definition = find_definition(&fresh.definitions, target, digest)?;
            commit_managed_configuration(
                self.tasks.as_ref(),
                self.config_store.clone(),
                definition.clone(),
                request,
                _lease,
                self.event_sender.clone(),
            )
            .await?;
            let snapshot = self
                .get_mcp_configuration(GetMcpConfigurationRequest::default())
                .await?
                .snapshot;
            return Ok(MutateMcpConfigurationResult { snapshot });
        }
        // 用户写入只修改用户配置文件；同名受管定义由加载优先级选择，不限制保存或导入。
        let runtime = self.registry.projections()?;
        self.config_store
            .mutate_with_runtime(&request.expected_revision, request.mutation, &runtime)
            .await?;
        let snapshot = self
            .get_mcp_configuration(GetMcpConfigurationRequest::default())
            .await?
            .snapshot;
        Ok(MutateMcpConfigurationResult { snapshot })
    }

    pub(super) async fn refresh_mcp_server(
        &self,
        request: assistant_protocol::RefreshMcpServerRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> RuntimeResult<McpRefreshControlResultSnapshot> {
        self.ensure_available()?;
        let view = self
            .get_mcp_configuration(GetMcpConfigurationRequest::default())
            .await?
            .snapshot;
        let server = view
            .servers
            .iter()
            .find(|server| {
                server.source == request.target.source
                    && server.server_key == request.target.server_key
            })
            .ok_or(RuntimeError::McpServerUnavailable)?;
        let (revision, digest) =
            server
                .managed
                .as_ref()
                .map_or((view.revision.as_str(), ""), |managed| {
                    (
                        managed.revision.as_str(),
                        managed.definition_digest.as_str(),
                    )
                });
        if revision != request.expected_revision || digest != request.expected_definition_digest {
            return Err(RuntimeError::McpConfigConflict);
        }
        // 完整目标仍核验编辑版本；同一执行 key 始终按全局优先级刷新，不能借低优先级入口抢占。
        refresh_mcp_registry_with(
            self.config_store.as_ref(),
            self.plugin.as_ref(),
            &self.builtin_definitions,
            self.registry.as_ref(),
            self.config_registry.as_ref(),
            self.approval_registry.as_ref(),
            &self.event_sender,
            &cancellation,
            Some(&request.target.server_key),
            Some(&request),
        )
        .await
    }

    /// 原插件设置提交后才刷新该插件的服务；连接失败不撤销已提交参数，不刷新其他来源。
    pub(super) async fn mutate_plugin_settings(
        &self,
        request: assistant_protocol::MutatePluginSettingsRequest,
    ) -> RuntimeResult<assistant_protocol::MutatePluginSettingsResult> {
        let _operation = self.operation_gate.read().await;
        if self.root_cancellation.is_cancelled() {
            return Err(RuntimeError::RuntimeNotRunning {
                lifecycle: assistant_protocol::RuntimeLifecycle::ShuttingDown,
            });
        }
        let result = self.plugin.mutate_settings(request).await?;
        if self.available {
            for name in &result.plugin.mcp {
                if let Ok(key) = assistant_protocol::McpServerKey::new(name.clone()) {
                    let _ = refresh_mcp_registry_with(
                        self.config_store.as_ref(),
                        self.plugin.as_ref(),
                        &self.builtin_definitions,
                        self.registry.as_ref(),
                        self.config_registry.as_ref(),
                        self.approval_registry.as_ref(),
                        &self.event_sender,
                        &self.root_cancellation,
                        Some(&key),
                        None,
                    )
                    .await;
                }
            }
        }
        Ok(result)
    }
}

#[path = "mcp/management_tools.rs"]
pub(super) mod management_tools;

/// 一次命令的来源事实归并，不新增目录缓存；相同内置定义用于查询、写前校验、测试及刷新。
async fn discover_mcp(
    plugin: &crate::plugin::PluginManager,
    builtin: &[crate::ManagedMcpServerDefinition],
) -> RuntimeResult<crate::plugin::PluginMcpDiscovery> {
    let mut discovery = plugin.mcp_candidates().await?;
    discovery.definitions.extend_from_slice(builtin);
    Ok(discovery)
}
