//! MCP 活动目录、候选交换与调用租约。

mod result;
use result::project_result;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_types::ToolResultContent;
#[cfg(test)]
use agent_types::ToolResultPart;
use assistant_protocol::{
    McpDiagnosticCode, McpDiagnosticSnapshot, McpRefreshControlResultSnapshot, McpRefreshOutcome,
    McpServerKey, McpServerRefreshOutcome, McpServerRefreshResultSnapshot, McpServerRuntimeState,
    McpToolIdentity, McpTransportKind,
};
use futures_util::{StreamExt as _, stream};
#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};
use tokio::sync::{Mutex, MutexGuard, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use super::{
    McpConnection, McpConnectionError, McpConnectionFactory, McpConnectionFailureKind,
    McpConnectionOptions, McpImageMaterializer, McpRegistryCandidate, McpServerConfig,
    McpToolDefinition, effective_connect_timeout, effective_tool_timeout,
    schema::{CompiledMcpTool, McpSchemaEngine, McpSchemaFailureKind},
};
use crate::{RuntimeError, RuntimeResult, config::McpRuntimeConfig};

const MAX_CATALOG_PAGES: usize = 64;
const MAX_TOOLS_PER_SERVER: usize = 256;
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_REFRESH_CONNECTIONS: usize = 4;
const SCHEMA_BLOCKING_CONCURRENCY: usize = 4;

#[derive(Clone, Copy)]
pub(crate) struct McpRegistryServerProjection {
    pub(crate) fingerprint: [u8; 32],
    pub(crate) state: McpServerRuntimeState,
    pub(crate) tool_count: u32,
}

/// Run 披露使用的当前内存目录快照；只复制业务说明和 Tool 定义，不携带连接或凭据。
#[derive(Clone)]
pub(crate) struct McpCatalogServer {
    pub(crate) server_key: McpServerKey,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) tools: Vec<McpToolDefinition>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ResolvedMcpInvocation {
    pub(crate) server_key: McpServerKey,
    pub(crate) server_display_name: String,
    pub(crate) tool_name: String,
    pub(crate) arguments: Map<String, Value>,
    pub(crate) untrusted_annotations: Option<Value>,
    // 只冻结连接实例身份，不持有调用 lease；审批不能把旧许可转移到同名新连接。
    #[serde(skip)]
    connection: Weak<McpServerState>,
}

impl ResolvedMcpInvocation {
    pub(crate) fn is_retired(&self) -> bool {
        self.connection
            .upgrade()
            .is_none_or(|state| state.retired.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test(server_key: McpServerKey, tool_name: String) -> Self {
        Self {
            server_display_name: server_key.as_str().to_owned(),
            server_key,
            tool_name,
            arguments: Map::new(),
            untrusted_annotations: None,
            connection: Weak::new(),
        }
    }
}

/// 目录已提交、旧连接待回收的短期资源 owner；调用者先取消旧审批，再等待回收。
/// refresh gate 随此值持有，避免另一次刷新越过本次的审批失效和诊断提交。
#[must_use]
pub(crate) struct McpRefreshCommit<'a> {
    pub(crate) result: McpRefreshControlResultSnapshot,
    retired: Vec<Arc<McpServerState>>,
    _refresh: MutexGuard<'a, ()>,
}

impl McpRefreshCommit<'_> {
    pub(crate) async fn finish(mut self) -> McpRefreshControlResultSnapshot {
        let results = stream::iter(self.retired)
            .map(|state| async move { (state.server_key.clone(), state.retire().await) })
            .buffer_unordered(MAX_REFRESH_CONNECTIONS)
            .collect::<Vec<_>>()
            .await;
        for (key, result) in results {
            if let Err(error) = result {
                if let Some(server) = self
                    .result
                    .servers
                    .iter_mut()
                    .find(|server| server.server_key == key)
                {
                    // 新目录已经生效，不能伪装成保留旧目录或回滚；回收失败单独形成诊断。
                    server.diagnostic = Some(connection_diagnostic(key, &error));
                }
                self.result.outcome = McpRefreshOutcome::Partial;
            }
        }
        self.result
    }
}

#[derive(Debug)]
pub(crate) struct McpCallProjection {
    pub(crate) content: ToolResultContent,
    pub(crate) is_error: bool,
    /// 本类型只在 `tools/call` 已返回后构造；后续 Schema/图片投影失败不能宣称远端未执行。
    pub(crate) remote_may_have_executed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpCallFailureKind {
    ServerUnavailable,
    CatalogChanged,
    InvalidInput,
    RequestFailed,
    UnsupportedResult,
    ResultLimit,
    Cancelled,
}

#[derive(Debug)]
pub(crate) struct McpCallFailure {
    pub(crate) kind: McpCallFailureKind,
    pub(crate) instance_path: Option<String>,
    pub(crate) keyword: Option<String>,
    pub(crate) remote_may_have_executed: bool,
}

/// 进程内 MCP 目录与连接引用的唯一权威 owner。
pub(crate) struct McpRegistry {
    refresh_gate: Mutex<()>,
    servers: RwLock<BTreeMap<McpServerKey, Arc<McpServerState>>>,
    diagnostics: RwLock<BTreeMap<McpServerKey, McpDiagnosticSnapshot>>,
    connection_factory: Arc<dyn McpConnectionFactory>,
    schema: McpSchemaEngine,
}

impl McpRegistry {
    pub(crate) fn new(connection_factory: Arc<dyn McpConnectionFactory>) -> Self {
        Self {
            refresh_gate: Mutex::new(()),
            servers: RwLock::new(BTreeMap::new()),
            diagnostics: RwLock::new(BTreeMap::new()),
            connection_factory,
            schema: McpSchemaEngine::new(SCHEMA_BLOCKING_CONCURRENCY),
        }
    }

    pub(crate) fn projections(
        &self,
    ) -> RuntimeResult<BTreeMap<McpServerKey, McpRegistryServerProjection>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| {
                servers
                    .iter()
                    .map(|(key, state)| {
                        (
                            key.clone(),
                            McpRegistryServerProjection {
                                fingerprint: state.fingerprint,
                                state: if state.tools.is_empty() {
                                    McpServerRuntimeState::ConnectedWithoutTools
                                } else {
                                    McpServerRuntimeState::Connected
                                },
                                tool_count: u32::try_from(state.tools.len()).unwrap_or(u32::MAX),
                            },
                        )
                    })
                    .collect()
            })
    }

    pub(crate) fn diagnostics(&self) -> RuntimeResult<Vec<McpDiagnosticSnapshot>> {
        let mut diagnostics: Vec<_> = self
            .diagnostics
            .read()
            .map_err(|_| registry_unavailable())
            .map(|diagnostics| diagnostics.values().cloned().collect())?;
        // 错误锁已释放；警告直接来自当前可用目录，刷新替换或删除后自然消失。
        for (key, server) in self
            .servers
            .read()
            .map_err(|_| registry_unavailable())?
            .iter()
        {
            diagnostics.extend(
                server
                    .tools
                    .values()
                    .filter_map(|tool| tool.description_warning(key)),
            );
        }
        Ok(diagnostics)
    }

    pub(crate) fn server_keys(&self) -> RuntimeResult<BTreeSet<McpServerKey>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| servers.keys().cloned().collect())
    }

    pub(crate) fn catalog_snapshot(&self) -> RuntimeResult<Vec<McpCatalogServer>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| {
                servers
                    .values()
                    .map(|server| McpCatalogServer {
                        server_key: server.server_key.clone(),
                        display_name: server.display_name.clone(),
                        description: server.description.clone(),
                        tools: server
                            .tools
                            .values()
                            .map(|tool| tool.definition.clone())
                            .collect(),
                    })
                    .collect()
            })
    }

    pub(crate) fn catalog_server(
        &self,
        server_key: &McpServerKey,
    ) -> RuntimeResult<Option<McpCatalogServer>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| {
                servers.get(server_key).map(|server| McpCatalogServer {
                    server_key: server.server_key.clone(),
                    display_name: server.display_name.clone(),
                    description: server.description.clone(),
                    tools: server
                        .tools
                        .values()
                        .map(|tool| tool.definition.clone())
                        .collect(),
                })
            })
    }

    pub(crate) fn tool_identities(&self) -> RuntimeResult<Vec<McpToolIdentity>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| {
                servers
                    .values()
                    .flat_map(|server| {
                        server.tools.keys().map(|tool_name| McpToolIdentity {
                            server_key: server.server_key.clone(),
                            server_display_name: server.display_name.clone(),
                            tool_name: tool_name.clone(),
                        })
                    })
                    .collect()
            })
    }

    pub(crate) fn tool_identity(
        &self,
        server_key: &McpServerKey,
        tool_name: &str,
    ) -> RuntimeResult<Option<McpToolIdentity>> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| {
                servers.get(server_key).and_then(|server| {
                    server
                        .tools
                        .contains_key(tool_name)
                        .then(|| McpToolIdentity {
                            server_key: server_key.clone(),
                            server_display_name: server.display_name.clone(),
                            tool_name: tool_name.to_owned(),
                        })
                })
            })
    }

    /// 完整候选建立后才替换当前 Arc。返回时目录已生效，调用者必须先使审批失效，
    /// 再 finish 回收旧连接；全过程不持有同步目录锁等待 I/O。旧 invocation 另按实例身份拒绝。
    pub(crate) async fn refresh(
        &self,
        candidate: McpRegistryCandidate,
        target: Option<&McpServerKey>,
        runtime: McpRuntimeConfig,
        cancellation: CancellationToken,
    ) -> RuntimeResult<McpRefreshCommit<'_>> {
        let _refresh = self.refresh_gate.lock().await;
        if !candidate.document_valid {
            return Err(RuntimeError::McpConfigInvalid);
        }

        let enabled = candidate
            .servers
            .values()
            .filter(|server| server.enabled())
            .filter(|server| target.is_none_or(|target| target == server.server_key()))
            .cloned()
            .collect::<Vec<_>>();
        let built = stream::iter(enabled)
            .map(|server| {
                let cancellation = cancellation.child_token();
                async move {
                    let key = server.server_key().clone();
                    let result = self.build_candidate(server, runtime, cancellation).await;
                    (key, result)
                }
            })
            .buffer_unordered(MAX_REFRESH_CONNECTIONS)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let current_keys = self
            .servers
            .read()
            .map_err(|_| registry_unavailable())?
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let keys = if let Some(target) = target {
            BTreeSet::from([target.clone()])
        } else {
            candidate
                .configured_keys
                .union(&current_keys)
                .cloned()
                .collect()
        };
        let configured_diagnostics = candidate
            .diagnostics
            .iter()
            .filter_map(|diagnostic| {
                diagnostic
                    .server_key
                    .as_ref()
                    .map(|key| (key.clone(), diagnostic.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let mut retired = Vec::new();
        let mut results = Vec::new();

        for key in keys {
            let configured = candidate.servers.get(&key);
            let is_configured = candidate.configured_keys.contains(&key);
            let current_count = self.current_tool_count(&key)?;
            let result = match configured {
                Some(server) if !server.enabled() => {
                    if let Some(old) = self.remove_current(&key)? {
                        retired.push(old);
                    }
                    self.clear_diagnostic(&key)?;
                    refresh_result(key, McpServerRefreshOutcome::Disabled, 0, None)
                }
                Some(_) => match built.get(&key) {
                    Some(Ok(next)) => {
                        let tool_count = next.tools.len();
                        if let Some(old) = self.swap_current(key.clone(), next.clone())? {
                            retired.push(old);
                        }
                        self.clear_diagnostic(&key)?;
                        let warning = next
                            .tools
                            .values()
                            .find_map(|tool| tool.description_warning(&key));
                        refresh_result(
                            key,
                            if tool_count == 0 {
                                McpServerRefreshOutcome::ConnectedWithoutTools
                            } else {
                                McpServerRefreshOutcome::Refreshed
                            },
                            tool_count,
                            warning,
                        )
                    }
                    Some(Err(diagnostic)) => {
                        self.set_diagnostic(key.clone(), diagnostic.clone())?;
                        refresh_result(
                            key,
                            McpServerRefreshOutcome::RetainedAfterFailure,
                            current_count,
                            Some(diagnostic.clone()),
                        )
                    }
                    None => {
                        let diagnostic = catalog_diagnostic(
                            key.clone(),
                            McpDiagnosticCode::CatalogFailed,
                            "MCP candidate was not built",
                        );
                        self.set_diagnostic(key.clone(), diagnostic.clone())?;
                        refresh_result(
                            key,
                            McpServerRefreshOutcome::RetainedAfterFailure,
                            current_count,
                            Some(diagnostic),
                        )
                    }
                },
                None if is_configured => {
                    let diagnostic =
                        configured_diagnostics
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| {
                                catalog_diagnostic(
                                    key.clone(),
                                    McpDiagnosticCode::InvalidConfig,
                                    "MCP server configuration is invalid",
                                )
                            });
                    self.set_diagnostic(key.clone(), diagnostic.clone())?;
                    refresh_result(
                        key,
                        McpServerRefreshOutcome::RetainedAfterFailure,
                        current_count,
                        Some(diagnostic),
                    )
                }
                None if current_keys.contains(&key) => {
                    if let Some(old) = self.remove_current(&key)? {
                        retired.push(old);
                    }
                    self.clear_diagnostic(&key)?;
                    refresh_result(key, McpServerRefreshOutcome::Removed, 0, None)
                }
                None => refresh_result(key, McpServerRefreshOutcome::NotFound, 0, None),
            };
            results.push(result);
        }

        Ok(McpRefreshCommit {
            result: McpRefreshControlResultSnapshot {
                outcome: overall_outcome(&results),
                servers: results,
            },
            retired,
            _refresh,
        })
    }

    #[cfg(test)]
    pub(crate) async fn resolve(
        &self,
        server_key: &McpServerKey,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ResolvedMcpInvocation, McpCallFailure> {
        let arguments = arguments.as_object().cloned().ok_or_else(invalid_input)?;
        let invocation = self.resolve_identity(server_key, tool_name, arguments)?;
        self.validate_invocation(&invocation).await?;
        Ok(invocation)
    }

    /// 只读取当前内存目录并冻结调用身份；不获取连接 lease，也不执行 blocking Schema 校验。
    /// Schema 校验由 Runtime 授权器在读取权限规则前通过 [`Self::validate_invocation`] 完成。
    pub(crate) fn resolve_identity(
        &self,
        server_key: &McpServerKey,
        tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<ResolvedMcpInvocation, McpCallFailure> {
        let state = self.current(server_key)?.ok_or_else(server_unavailable)?;
        let tool = state
            .tools
            .get(tool_name)
            .cloned()
            .ok_or_else(catalog_changed)?;
        Ok(ResolvedMcpInvocation {
            server_key: server_key.clone(),
            server_display_name: state.display_name.clone(),
            tool_name: tool_name.to_owned(),
            arguments,
            untrusted_annotations: tool.definition.annotations.clone(),
            connection: Arc::downgrade(&state),
        })
    }

    /// 在审批和远端副作用前，以当前目录的 compiled Schema 校验冻结参数。
    pub(crate) async fn validate_invocation(
        &self,
        invocation: &ResolvedMcpInvocation,
    ) -> Result<(), McpCallFailure> {
        let state = self.invocation_state(invocation)?;
        let tool = state
            .tools
            .get(&invocation.tool_name)
            .cloned()
            .ok_or_else(catalog_changed)?;
        tool.validate_input(&self.schema, Value::Object(invocation.arguments.clone()))
            .await
            .map_err(|failure| McpCallFailure {
                kind: McpCallFailureKind::InvalidInput,
                instance_path: Some(failure.instance_path),
                keyword: Some(failure.keyword),
                remote_may_have_executed: false,
            })
    }

    fn invocation_state(
        &self,
        invocation: &ResolvedMcpInvocation,
    ) -> Result<Arc<McpServerState>, McpCallFailure> {
        let current = self
            .current(&invocation.server_key)?
            .ok_or_else(server_unavailable)?;
        let resolved = invocation
            .connection
            .upgrade()
            .ok_or_else(catalog_changed)?;
        if !Arc::ptr_eq(&current, &resolved) || resolved.retired.load(Ordering::Acquire) {
            return Err(catalog_changed());
        }
        Ok(resolved)
    }

    pub(crate) async fn execute(
        &self,
        invocation: &ResolvedMcpInvocation,
        session_tool_image_directory: Option<&str>,
        image_materializer: &dyn McpImageMaterializer,
        cancellation: CancellationToken,
    ) -> Result<McpCallProjection, McpCallFailure> {
        // Run 取消应传递到调用，但工具超时不能反向取消整个 Run；后续模型仍需收到
        // remote_may_have_executed 错误并决定下一步，不得在这里重放远端请求。
        let cancellation = cancellation.child_token();
        let state = self.invocation_state(invocation)?;
        let tool = state
            .tools
            .get(&invocation.tool_name)
            .cloned()
            .ok_or_else(catalog_changed)?;
        tool.validate_input(&self.schema, Value::Object(invocation.arguments.clone()))
            .await
            .map_err(|failure| McpCallFailure {
                kind: McpCallFailureKind::CatalogChanged,
                instance_path: Some(failure.instance_path),
                keyword: Some(failure.keyword),
                remote_may_have_executed: false,
            })?;
        let lease = state.acquire_lease().ok_or_else(catalog_changed)?;
        let permit = tokio::select! {
            () = cancellation.cancelled() => return Err(cancelled(false)),
            permit = state.call_limit.clone().acquire_owned() => {
                permit.map_err(|_| server_unavailable())?
            }
        };
        let call = state.connection.call_tool_once(
            invocation.tool_name.clone(),
            invocation.arguments.clone(),
            cancellation.clone(),
        );
        let raw = match tokio::time::timeout(state.request_timeout, call).await {
            Ok(Ok(raw)) => raw,
            Ok(Err(error)) => {
                let failure = call_failure(&error);
                self.mark_unavailable_if_current(&state, &error)
                    .map_err(|_| server_unavailable())?;
                drop(permit);
                drop(lease);
                if let Err(cleanup) = state.retire().await {
                    self.set_diagnostic(
                        state.server_key.clone(),
                        connection_diagnostic(state.server_key.clone(), &cleanup),
                    )
                    .map_err(|_| server_unavailable())?;
                }
                return Err(failure);
            }
            Err(_) => {
                cancellation.cancel();
                drop(permit);
                drop(lease);
                return Err(McpCallFailure {
                    kind: McpCallFailureKind::RequestFailed,
                    instance_path: None,
                    keyword: None,
                    remote_may_have_executed: true,
                });
            }
        };
        let projection = project_result(
            raw,
            &tool,
            &self.schema,
            session_tool_image_directory,
            image_materializer,
            &cancellation,
        )
        .await;
        drop(permit);
        drop(lease);
        projection
    }

    pub(crate) async fn shutdown(&self) -> RuntimeResult<()> {
        let _refresh = self.refresh_gate.lock().await;
        let retired = {
            let mut servers = self.servers.write().map_err(|_| registry_unavailable())?;
            let retired = std::mem::take(&mut *servers)
                .into_values()
                .collect::<Vec<_>>();
            for state in &retired {
                state.mark_retired();
            }
            retired
        };
        let results = stream::iter(retired)
            .map(|state| async move { state.retire().await })
            .buffer_unordered(MAX_REFRESH_CONNECTIONS)
            .collect::<Vec<_>>()
            .await;
        if results.iter().any(Result::is_err) {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "MCP connection cleanup",
            });
        }
        Ok(())
    }

    async fn build_candidate(
        &self,
        server: McpServerConfig,
        runtime: McpRuntimeConfig,
        cancellation: CancellationToken,
    ) -> Result<Arc<McpServerState>, McpDiagnosticSnapshot> {
        let key = server.server_key().clone();
        let connection = match tokio::time::timeout(
            effective_connect_timeout(runtime, &server),
            self.connection_factory.connect(
                server.clone(),
                McpConnectionOptions {
                    max_concurrent_requests: usize::try_from(
                        runtime.max_concurrent_calls_per_server().get(),
                    )
                    .unwrap_or(1),
                    control_timeout: runtime.close_timeout(),
                },
                cancellation.clone(),
            ),
        )
        .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => return Err(connection_diagnostic(key, &error)),
            Err(_) => {
                cancellation.cancel();
                return Err(catalog_diagnostic(
                    key,
                    McpDiagnosticCode::ConnectFailed,
                    "MCP connection timed out",
                ));
            }
        };
        let catalog = tokio::time::timeout(
            runtime.catalog_timeout(),
            self.build_catalog(&key, connection.clone(), cancellation.clone()),
        )
        .await;
        let tools = match catalog {
            Ok(Ok(tools)) => tools,
            Ok(Err(diagnostic)) => {
                if let Err(error) = close_connection(connection, runtime.close_timeout()).await {
                    return Err(connection_diagnostic(key, &error));
                }
                return Err(diagnostic);
            }
            Err(_) => {
                cancellation.cancel();
                if let Err(error) = close_connection(connection, runtime.close_timeout()).await {
                    return Err(connection_diagnostic(key, &error));
                }
                return Err(catalog_diagnostic(
                    key,
                    McpDiagnosticCode::CatalogFailed,
                    "MCP catalog compilation timed out",
                ));
            }
        };
        Ok(Arc::new(McpServerState {
            server_key: server.server_key().clone(),
            display_name: server.display_name().to_owned(),
            description: server.description().to_owned(),
            transport: server.transport_kind(),
            fingerprint: server.fingerprint(),
            request_timeout: effective_tool_timeout(runtime, &server),
            close_timeout: runtime.close_timeout(),
            connection,
            tools,
            call_limit: Arc::new(Semaphore::new(
                usize::try_from(runtime.max_concurrent_calls_per_server().get()).unwrap_or(1),
            )),
            active_leases: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            lease_released: Notify::new(),
        }))
    }

    /// 测试和刷新共用完整目录校验；不写配置、不交换活动连接、不修改诊断。
    /// 调用者负责超时、取消及候选连接关闭；两条路径共用同一 blocking 并发上限。
    pub(crate) async fn build_catalog(
        &self,
        key: &McpServerKey,
        connection: Arc<dyn McpConnection>,
        cancellation: CancellationToken,
    ) -> Result<BTreeMap<String, Arc<CompiledMcpTool>>, McpDiagnosticSnapshot> {
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut tools = BTreeMap::new();
        let mut catalog_bytes = 0usize;
        for _ in 0..MAX_CATALOG_PAGES {
            let page = connection
                .list_tools_page(cursor, cancellation.clone())
                .await
                .map_err(|error| connection_diagnostic(key.clone(), &error))?;
            for definition in page.tools {
                if tools.len() >= MAX_TOOLS_PER_SERVER {
                    return Err(catalog_diagnostic(
                        key.clone(),
                        McpDiagnosticCode::LimitExceeded,
                        "MCP server exceeds the tool limit",
                    ));
                }
                if tools.contains_key(&definition.name) {
                    return Err(catalog_diagnostic(
                        key.clone(),
                        McpDiagnosticCode::CatalogFailed,
                        "MCP server returned a duplicate tool name",
                    ));
                }
                catalog_bytes = catalog_bytes.saturating_add(definition_size(&definition));
                if catalog_bytes > MAX_CATALOG_BYTES {
                    return Err(catalog_diagnostic(
                        key.clone(),
                        McpDiagnosticCode::LimitExceeded,
                        "MCP tool catalog exceeds the size limit",
                    ));
                }
                let name = definition.name.clone();
                let compiled = self
                    .schema
                    .compile_tool(definition)
                    .await
                    .map_err(|failure| {
                        catalog_diagnostic(
                            key.clone(),
                            if failure.kind == McpSchemaFailureKind::Limit {
                                McpDiagnosticCode::LimitExceeded
                            } else {
                                McpDiagnosticCode::SchemaInvalid
                            },
                            failure.message,
                        )
                    })?;
                tools.insert(name, Arc::new(compiled));
            }
            let Some(next) = page.next_cursor else {
                return Ok(tools);
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(catalog_diagnostic(
                    key.clone(),
                    McpDiagnosticCode::CatalogFailed,
                    "MCP server repeated a catalog cursor",
                ));
            }
            cursor = Some(next);
        }
        Err(catalog_diagnostic(
            key.clone(),
            McpDiagnosticCode::LimitExceeded,
            "MCP server exceeds the catalog page limit",
        ))
    }

    fn current(&self, key: &McpServerKey) -> Result<Option<Arc<McpServerState>>, McpCallFailure> {
        self.servers
            .read()
            .map_err(|_| server_unavailable())
            .map(|servers| servers.get(key).cloned())
    }

    fn current_tool_count(&self, key: &McpServerKey) -> RuntimeResult<usize> {
        self.servers
            .read()
            .map_err(|_| registry_unavailable())
            .map(|servers| servers.get(key).map_or(0, |state| state.tools.len()))
    }

    fn swap_current(
        &self,
        key: McpServerKey,
        next: Arc<McpServerState>,
    ) -> RuntimeResult<Option<Arc<McpServerState>>> {
        let mut servers = self.servers.write().map_err(|_| registry_unavailable())?;
        if let Some(old) = servers.get(&key) {
            old.mark_retired();
        }
        Ok(servers.insert(key, next))
    }

    fn remove_current(&self, key: &McpServerKey) -> RuntimeResult<Option<Arc<McpServerState>>> {
        let mut servers = self.servers.write().map_err(|_| registry_unavailable())?;
        if let Some(old) = servers.get(key) {
            old.mark_retired();
        }
        Ok(servers.remove(key))
    }

    fn mark_unavailable_if_current(
        &self,
        failed: &Arc<McpServerState>,
        error: &McpConnectionError,
    ) -> RuntimeResult<()> {
        let removed = {
            let mut servers = self.servers.write().map_err(|_| registry_unavailable())?;
            if servers
                .get(&failed.server_key)
                .is_some_and(|current| Arc::ptr_eq(current, failed))
            {
                let removed = servers.remove(&failed.server_key);
                failed.mark_retired();
                removed
            } else {
                None
            }
        };
        if removed.is_some() {
            self.set_diagnostic(
                failed.server_key.clone(),
                connection_diagnostic(failed.server_key.clone(), error),
            )?;
        }
        Ok(())
    }

    fn set_diagnostic(
        &self,
        key: McpServerKey,
        diagnostic: McpDiagnosticSnapshot,
    ) -> RuntimeResult<()> {
        self.diagnostics
            .write()
            .map_err(|_| registry_unavailable())?
            .insert(key, diagnostic);
        Ok(())
    }

    fn clear_diagnostic(&self, key: &McpServerKey) -> RuntimeResult<()> {
        self.diagnostics
            .write()
            .map_err(|_| registry_unavailable())?
            .remove(key);
        Ok(())
    }
}

struct McpServerState {
    server_key: McpServerKey,
    display_name: String,
    description: String,
    #[allow(dead_code)]
    transport: McpTransportKind,
    fingerprint: [u8; 32],
    request_timeout: Duration,
    close_timeout: Duration,
    connection: Arc<dyn McpConnection>,
    tools: BTreeMap<String, Arc<CompiledMcpTool>>,
    call_limit: Arc<Semaphore>,
    active_leases: AtomicUsize,
    retired: AtomicBool,
    lease_released: Notify,
}

impl McpServerState {
    fn acquire_lease(self: &Arc<Self>) -> Option<McpServerLease> {
        if self.retired.load(Ordering::Acquire) {
            return None;
        }
        self.active_leases.fetch_add(1, Ordering::AcqRel);
        if self.retired.load(Ordering::Acquire) {
            self.release_lease();
            return None;
        }
        Some(McpServerLease {
            state: self.clone(),
        })
    }

    fn mark_retired(&self) {
        self.retired.store(true, Ordering::Release);
        self.call_limit.close();
        self.lease_released.notify_waiters();
    }

    fn release_lease(&self) {
        if self.active_leases.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.lease_released.notify_waiters();
        }
    }

    async fn retire(&self) -> Result<(), McpConnectionError> {
        self.mark_retired();
        loop {
            let notified = self.lease_released.notified();
            if self.active_leases.load(Ordering::Acquire) == 0 {
                break;
            }
            notified.await;
        }
        close_connection(self.connection.clone(), self.close_timeout).await
    }
}

struct McpServerLease {
    state: Arc<McpServerState>,
}

impl Drop for McpServerLease {
    fn drop(&mut self) {
        self.state.release_lease();
    }
}

async fn close_connection(
    connection: Arc<dyn McpConnection>,
    timeout: Duration,
) -> Result<(), McpConnectionError> {
    let cancellation = CancellationToken::new();
    match tokio::time::timeout(timeout, connection.close(cancellation.clone())).await {
        Ok(result) => result,
        Err(_) => {
            cancellation.cancel();
            Err(McpConnectionError::new(
                McpConnectionFailureKind::Close,
                "MCP connection cleanup timed out",
            ))
        }
    }
}

fn definition_size(definition: &McpToolDefinition) -> usize {
    definition
        .name
        .len()
        .saturating_add(definition.title.as_ref().map_or(0, String::len))
        .saturating_add(definition.description.as_ref().map_or(0, String::len))
        .saturating_add(encoded_len(&definition.input_schema))
        .saturating_add(definition.output_schema.as_ref().map_or(0, encoded_len))
        .saturating_add(definition.annotations.as_ref().map_or(0, encoded_len))
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |encoded| encoded.len())
}

fn overall_outcome(results: &[McpServerRefreshResultSnapshot]) -> McpRefreshOutcome {
    let failures = results
        .iter()
        .filter(|result| {
            matches!(
                result.outcome,
                McpServerRefreshOutcome::RetainedAfterFailure | McpServerRefreshOutcome::NotFound
            )
        })
        .count();
    if failures == 0 {
        McpRefreshOutcome::Success
    } else if failures == results.len() {
        McpRefreshOutcome::Failure
    } else {
        McpRefreshOutcome::Partial
    }
}

fn refresh_result(
    server_key: McpServerKey,
    outcome: McpServerRefreshOutcome,
    tool_count: usize,
    diagnostic: Option<McpDiagnosticSnapshot>,
) -> McpServerRefreshResultSnapshot {
    McpServerRefreshResultSnapshot {
        server_key,
        outcome,
        tool_count: u32::try_from(tool_count).unwrap_or(u32::MAX),
        diagnostic,
    }
}

fn catalog_diagnostic(
    server_key: McpServerKey,
    code: McpDiagnosticCode,
    message: &'static str,
) -> McpDiagnosticSnapshot {
    McpDiagnosticSnapshot {
        server_key: Some(server_key),
        code,
        field_path: None,
        message: message.to_owned(),
    }
}

fn connection_diagnostic(
    server_key: McpServerKey,
    error: &McpConnectionError,
) -> McpDiagnosticSnapshot {
    let code = match error.kind() {
        McpConnectionFailureKind::InvalidConfig => McpDiagnosticCode::InvalidConfig,
        McpConnectionFailureKind::Connect | McpConnectionFailureKind::Cancelled => {
            McpDiagnosticCode::ConnectFailed
        }
        McpConnectionFailureKind::Protocol
        | McpConnectionFailureKind::Close
        | McpConnectionFailureKind::UnsupportedExtension => McpDiagnosticCode::ProtocolFailed,
        McpConnectionFailureKind::Catalog | McpConnectionFailureKind::ToolCall => {
            McpDiagnosticCode::CatalogFailed
        }
    };
    catalog_diagnostic(server_key, code, error.message())
}

fn call_failure(error: &McpConnectionError) -> McpCallFailure {
    McpCallFailure {
        kind: if error.kind() == McpConnectionFailureKind::Cancelled {
            McpCallFailureKind::Cancelled
        } else if error.kind() == McpConnectionFailureKind::UnsupportedExtension {
            McpCallFailureKind::UnsupportedResult
        } else {
            McpCallFailureKind::RequestFailed
        },
        instance_path: None,
        keyword: None,
        remote_may_have_executed: true,
    }
}

#[cfg(test)]
fn invalid_input() -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::InvalidInput,
        instance_path: None,
        keyword: None,
        remote_may_have_executed: false,
    }
}

fn catalog_changed() -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::CatalogChanged,
        instance_path: None,
        keyword: None,
        remote_may_have_executed: false,
    }
}

fn server_unavailable() -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::ServerUnavailable,
        instance_path: None,
        keyword: None,
        remote_may_have_executed: false,
    }
}

fn cancelled(remote_may_have_executed: bool) -> McpCallFailure {
    McpCallFailure {
        kind: McpCallFailureKind::Cancelled,
        instance_path: None,
        keyword: None,
        remote_may_have_executed,
    }
}

fn registry_unavailable() -> RuntimeError {
    RuntimeError::InternalStateUnavailable {
        component: "MCP registry",
    }
}

#[cfg(test)]
mod tests;
