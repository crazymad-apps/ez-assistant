//! 插件安装操作的用户域权威编排。文件和网络副作用由 Host 端口完成。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    BeginPluginInstallRequest, IdempotencyKey, InstalledPluginSnapshot, McpDiagnosticSnapshot,
    MutatePluginSettingsRequest, MutatePluginSettingsResult, PluginConfigurationFieldSnapshot,
    PluginListSnapshot, PluginOperationRequest, PluginOperationSnapshot, PluginOperationStatus,
    PluginPackageOrigin, PluginPackageSelection, PluginPackageState, PluginSettingsMutation,
    PluginSoftwareFieldSnapshot, RuntimeEvent, UninstallPluginRequest,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio_util::sync::CancellationToken;

use crate::observation::ObservationCoordinator;
use crate::runtime::tasks::RuntimeTasks;
use crate::{
    PluginOperationKind, PluginOperationPhase, PluginOperationUpdate, PluginRecordChange,
    PluginSettingsChange, PluginSource, RuntimeError, RuntimeResult, RuntimeStore, StoredPlugin,
    StoredPluginOperation,
};

/// Host 校验后交回的最小包事实；不把文件路径或原始归档交给 Runtime。
pub struct PreparedPluginPackage {
    pub manifest_json: String,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginFileError {
    #[error("plugin package scanner is busy")]
    Busy,
    #[error("plugin package is invalid")]
    Invalid,
    #[error("plugin package changed")]
    Changed,
    #[error("plugin package operation was cancelled")]
    Cancelled,
    #[error("plugin package file is unavailable")]
    Unavailable,
    #[error("plugin cleanup requires attention")]
    CleanupRequired,
}

/// Host 只执行明确的单包文件/网络动作；是否生效始终由 Store 事务决定。
pub trait PluginFilePort: Send + Sync {
    fn mcp_candidates(
        &self,
        _plugins: Vec<StoredPlugin>,
    ) -> BoxFuture<'_, Result<PluginMcpDiscovery, PluginFileError>> {
        Box::pin(async { Ok(PluginMcpDiscovery::default()) })
    }
    fn prepare(
        &self,
        key: IdempotencyKey,
        selection: PluginPackageSelection,
        cancellation: CancellationToken,
        progress: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> BoxFuture<'_, Result<PreparedPluginPackage, PluginFileError>>;
    fn publish(
        &self,
        key: IdempotencyKey,
        selection: PluginPackageSelection,
    ) -> BoxFuture<'_, Result<(), PluginFileError>>;
    /// 只删除由 key/name/sha 明确归属的未生效准备内容。
    fn discard(
        &self,
        key: IdempotencyKey,
        selection: Option<PluginPackageSelection>,
    ) -> BoxFuture<'_, Result<(), PluginFileError>>;
    /// DB 已提交以后收尾旧包和过程目录；重复调用必须安全。
    fn finish(
        &self,
        key: IdempotencyKey,
        change: CommittedPluginChange,
    ) -> BoxFuture<'_, Result<(), PluginFileError>>;
    /// 检查已生效引用及全部可识别过程目录；不能凭目录推断安装事实。
    fn audit(
        &self,
        plugins: Vec<StoredPlugin>,
        operations: Vec<StoredPluginOperation>,
    ) -> BoxFuture<'_, Result<PluginAudit, PluginFileError>>;
}

#[derive(Default)]
pub struct PluginMcpDiscovery {
    pub servers: Vec<crate::McpServerConfig>,
    pub diagnostics: Vec<McpDiagnosticSnapshot>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginAudit {
    pub corrupt_names: BTreeSet<String>,
    pub cleanup_blocked: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommittedPluginChange {
    pub name: String,
    pub previous_sha256: Option<String>,
    pub next_sha256: Option<String>,
    pub clear_data: bool,
}

/// 后续 Skill/MCP 装配捕获当前包时持有，直到受管 Run 或调用结束。
pub struct PluginUseLease {
    pub name: String,
    pub sha256: String,
    _guard: OwnedRwLockReadGuard<()>,
}

pub struct PluginConfigLease {
    pub name: String,
    pub sha256: String,
    _guard: OwnedRwLockWriteGuard<()>,
}

pub(crate) struct PluginManager {
    store: Arc<dyn RuntimeStore>,
    tasks: Arc<RuntimeTasks>,
    cancellation: CancellationToken,
    admission: Mutex<()>,
    commit: Mutex<()>,
    active: Mutex<Option<(IdempotencyKey, CancellationToken)>>,
    uploads: Mutex<BTreeSet<IdempotencyKey>>,
    use_gates: Mutex<BTreeMap<String, Arc<RwLock<()>>>>,
    progress: Arc<std::sync::Mutex<BTreeMap<IdempotencyKey, (u64, u64)>>>,
    progress_notice: Arc<std::sync::Mutex<BTreeMap<IdempotencyKey, Instant>>>,
    audit_state: std::sync::Mutex<PluginAudit>,
    mcp_retirement: std::sync::RwLock<
        Option<(
            std::sync::Weak<crate::mcp::McpRegistry>,
            Arc<crate::permission::ApprovalRegistry>,
        )>,
    >,
    events: ObservationCoordinator,
    files: Option<Arc<dyn PluginFilePort>>,
}

impl PluginManager {
    pub(crate) async fn mcp_candidates(&self) -> RuntimeResult<PluginMcpDiscovery> {
        let corrupt = self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .clone();
        let plugins = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .filter(|plugin| {
                plugin.enabled && plugin.active_sha256.is_some() && !corrupt.contains(&plugin.name)
            })
            .collect::<Vec<_>>();
        if plugins.is_empty() {
            return Ok(PluginMcpDiscovery::default());
        }
        self.files()?
            .mcp_candidates(plugins)
            .await
            .map_err(|_| RuntimeError::PluginPackageUnavailable)
    }

    pub(crate) async fn declared_mcp_keys(
        &self,
    ) -> RuntimeResult<BTreeSet<assistant_protocol::McpServerKey>> {
        let mut keys = BTreeSet::new();
        for plugin in self.store.list_plugins().await.map_err(storage)? {
            if !plugin.enabled || plugin.active_sha256.is_none() {
                continue;
            }
            let Some(manifest) = plugin
                .active_manifest_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            else {
                continue;
            };
            if let Some(names) = manifest.get("mcp").and_then(serde_json::Value::as_array) {
                for name in names {
                    if let Some(name) = name.as_str()
                        && let Ok(key) = assistant_protocol::McpServerKey::new(name.to_owned())
                    {
                        keys.insert(key);
                    }
                }
            }
        }
        Ok(keys)
    }
    pub(crate) async fn active_skill_packages(
        &self,
    ) -> RuntimeResult<Vec<crate::PluginSkillPackage>> {
        let corrupt = self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .clone();
        Ok(self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .filter(|plugin| plugin.enabled && !corrupt.contains(&plugin.name))
            .filter_map(|plugin| {
                plugin
                    .active_sha256
                    .map(|sha256| crate::PluginSkillPackage {
                        name: plugin.name,
                        sha256,
                    })
            })
            .collect())
    }
    pub(crate) fn new(
        store: Arc<dyn RuntimeStore>,
        tasks: Arc<RuntimeTasks>,
        cancellation: CancellationToken,
        events: ObservationCoordinator,
    ) -> Self {
        Self {
            store,
            tasks,
            cancellation,
            admission: Mutex::new(()),
            commit: Mutex::new(()),
            active: Mutex::new(None),
            uploads: Mutex::new(BTreeSet::new()),
            use_gates: Mutex::new(BTreeMap::new()),
            progress: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            progress_notice: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            audit_state: std::sync::Mutex::new(PluginAudit::default()),
            mcp_retirement: std::sync::RwLock::new(None),
            events,
            files: None,
        }
    }

    pub(crate) fn with_files(mut self, files: Arc<dyn PluginFilePort>) -> Self {
        self.files = Some(files);
        self
    }

    pub(crate) fn bind_mcp_retirement(
        &self,
        registry: &Arc<crate::mcp::McpRegistry>,
        approvals: Arc<crate::permission::ApprovalRegistry>,
    ) {
        *self
            .mcp_retirement
            .write()
            .expect("plugin MCP retirement poisoned") = Some((Arc::downgrade(registry), approvals));
    }

    async fn retire_old_mcp(&self, name: &str, sha256: &str) -> RuntimeResult<()> {
        let projection = self
            .mcp_retirement
            .read()
            .expect("plugin MCP retirement poisoned")
            .clone();
        let Some((registry, approvals)) = projection else {
            return Ok(());
        };
        let Some(registry) = registry.upgrade() else {
            return Err(RuntimeError::PluginBusy);
        };
        let (keys, close_failed) = registry.retire_plugin_package(name, sha256).await?;
        for key in keys {
            for approval in approvals.invalidate_pending_mcp_server(&key)? {
                let _ = self.events.send(RuntimeEvent::ApprovalCancelled {
                    session_id: approval.session_id,
                    run_id: approval.run_id,
                    child_task_id: approval.child_task_id,
                    approval_id: approval.approval_id,
                });
            }
        }
        if close_failed {
            Err(RuntimeError::PluginBusy)
        } else {
            Ok(())
        }
    }

    fn files(&self) -> RuntimeResult<Arc<dyn PluginFilePort>> {
        self.files.clone().ok_or(RuntimeError::StorageUnavailable {
            operation: "access plugin files",
            source: None,
        })
    }

    pub(crate) async fn list(&self) -> RuntimeResult<PluginListSnapshot> {
        let plugins = self.store.list_plugins().await.map_err(storage)?;
        let mut operations = self.store.list_plugin_operations().await.map_err(storage)?;
        operations.sort_by_key(|operation| std::cmp::Reverse(operation.updated_at_ms));
        let mut terminal_count = 0;
        let operations = operations
            .into_iter()
            .filter(|operation| {
                if !matches!(
                    operation.phase,
                    PluginOperationPhase::Succeeded
                        | PluginOperationPhase::Failed
                        | PluginOperationPhase::Cancelled
                        | PluginOperationPhase::Interrupted
                ) || operation.error_code.as_deref() == Some("cleanup_required")
                {
                    return true;
                }
                terminal_count += 1;
                terminal_count <= 8
            })
            .map(|operation| snapshot(&operation))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let audit = self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .clone();
        Ok(PluginListSnapshot {
            plugins: plugins
                .into_iter()
                .map(|plugin| {
                    let package_state = if audit.corrupt_names.contains(&plugin.name) {
                        PluginPackageState::Corrupt
                    } else {
                        PluginPackageState::Available
                    };
                    project_plugin(plugin, package_state)
                })
                .collect(),
            cleanup_blocked: audit.cleanup_blocked,
            operations,
        })
    }

    pub(crate) async fn mutate_settings(
        &self,
        request: MutatePluginSettingsRequest,
    ) -> RuntimeResult<MutatePluginSettingsResult> {
        let _admission = self.admission.lock().await;
        if self.active.lock().await.is_some() {
            return Err(RuntimeError::PluginBusy);
        }
        let current = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|plugin| plugin.name == request.name)
            .ok_or(RuntimeError::PluginPackageUnavailable)?;
        if current.active_sha256.as_deref() != Some(&request.expected_active_sha256)
            || settings_revision(&current) != request.expected_revision
        {
            return Err(RuntimeError::PluginBusy);
        }
        let mut next = current.clone();
        match request.mutation {
            PluginSettingsMutation::SetEnabled { enabled } => next.enabled = enabled,
            PluginSettingsMutation::SetParameter { name, value } => {
                if !declared_field(&current, "configuration", &name) {
                    return Err(invalid());
                }
                change_setting(&mut next.parameters_json, name, value)?;
            }
            PluginSettingsMutation::SetSoftwarePath { name, value } => {
                if !declared_field(&current, "external_software", &name)
                    || value
                        .as_deref()
                        .is_some_and(|path| !std::path::Path::new(path).is_absolute())
                {
                    return Err(invalid());
                }
                change_setting(&mut next.software_paths_json, name, value)?;
            }
        }
        if next.enabled == current.enabled
            && next.parameters_json == current.parameters_json
            && next.software_paths_json == current.software_paths_json
        {
            return Ok(MutatePluginSettingsResult {
                plugin: project_plugin(current, PluginPackageState::Available),
            });
        }
        next.updated_at_ms = now_ms().max(current.updated_at_ms.saturating_add(1));
        let next = self
            .store
            .change_plugin_settings(PluginSettingsChange {
                expected: current,
                next,
            })
            .await
            .map_err(storage)?;
        let _ = self.events.send(RuntimeEvent::PluginCatalogChanged);
        Ok(MutatePluginSettingsResult {
            plugin: project_plugin(next, PluginPackageState::Available),
        })
    }

    pub(crate) async fn get(&self, key: &IdempotencyKey) -> RuntimeResult<PluginOperationSnapshot> {
        let operation = self.load(key).await?;
        let mut result = snapshot(&operation)?;
        if let Some((received, total)) = self
            .progress
            .lock()
            .expect("plugin progress poisoned")
            .get(key)
        {
            result.received_bytes = Some(*received);
            result.total_bytes = Some(*total);
        }
        Ok(result)
    }

    pub(crate) fn report_progress(&self, key: &IdempotencyKey, received: u64, total: u64) {
        report_progress(
            &self.progress,
            &self.progress_notice,
            &self.events,
            key,
            received,
            total,
        );
    }

    pub(crate) async fn begin_install(
        self: &Arc<Self>,
        request: BeginPluginInstallRequest,
    ) -> RuntimeResult<PluginOperationSnapshot> {
        validate_install(&request)?;
        let _admission = self.admission.lock().await;
        let intent_json = serde_json::to_string(&request).map_err(|_| invalid())?;
        let existing = self
            .store
            .list_plugin_operations()
            .await
            .map_err(storage)?
            .iter()
            .any(|operation| operation.key == request.key);
        if !existing {
            if self.active.lock().await.is_some() {
                return Err(RuntimeError::PluginBusy);
            }
            self.ensure_no_unfinished_except(&request.key).await?;
            self.refresh_audit().await?;
        }
        let operation = self
            .accept(
                request.key.clone(),
                PluginOperationKind::Install,
                intent_json,
            )
            .await?;
        if operation.phase == PluginOperationPhase::Accepted
            && request
                .packages
                .iter()
                .all(|item| matches!(item.origin, PluginPackageOrigin::Center { .. }))
        {
            self.spawn_install(request.key.clone()).await?;
        }
        snapshot(&operation)
    }

    /// 上传路由收到并同步原包之后调用；重复请求只回读已存收据。
    pub(crate) async fn uploaded(
        self: &Arc<Self>,
        key: &IdempotencyKey,
    ) -> RuntimeResult<PluginOperationSnapshot> {
        let _commit = self.commit.lock().await;
        self.uploads.lock().await.remove(key);
        let operation = self.load(key).await?;
        let intent: BeginPluginInstallRequest =
            serde_json::from_str(&operation.intent_json).map_err(|_| invalid())?;
        if operation.kind != PluginOperationKind::Install
            || intent.packages.len() != 1
            || !matches!(intent.packages[0].origin, PluginPackageOrigin::Upload)
        {
            return Err(invalid());
        }
        if operation.phase == PluginOperationPhase::Accepted {
            self.spawn_install(key.clone()).await?;
        }
        self.get(key).await
    }

    pub(crate) async fn upload_selection(
        &self,
        key: &IdempotencyKey,
    ) -> RuntimeResult<PluginPackageSelection> {
        let _commit = self.commit.lock().await;
        let operation = self.load(key).await?;
        let intent: BeginPluginInstallRequest =
            serde_json::from_str(&operation.intent_json).map_err(|_| invalid())?;
        if operation.phase != PluginOperationPhase::Accepted
            || operation.cancel_requested
            || operation.kind != PluginOperationKind::Install
            || intent.packages.len() != 1
            || !matches!(intent.packages[0].origin, PluginPackageOrigin::Upload)
        {
            return Err(invalid());
        }
        if !self.uploads.lock().await.insert(key.clone()) {
            return Err(RuntimeError::PluginBusy);
        }
        Ok(intent
            .packages
            .into_iter()
            .next()
            .expect("single upload package"))
    }

    pub(crate) async fn upload_failed(
        &self,
        key: &IdempotencyKey,
        cancelled: bool,
    ) -> RuntimeResult<()> {
        self.uploads.lock().await.remove(key);
        let operation = self.load(key).await?;
        if operation.phase == PluginOperationPhase::Accepted {
            self.stop_before_commit(key, None, cancelled).await?;
        }
        Ok(())
    }

    pub(crate) async fn use_package(&self, name: &str) -> RuntimeResult<PluginUseLease> {
        if self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .contains(name)
        {
            return Err(RuntimeError::PluginPackageUnavailable);
        }
        let before = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|plugin| plugin.name == name && plugin.enabled)
            .and_then(|plugin| plugin.active_sha256)
            .ok_or(RuntimeError::PluginPackageUnavailable)?;
        let gate = self.use_gate(name).await;
        let guard = gate.read_owned().await;
        let after = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|plugin| plugin.name == name && plugin.enabled)
            .and_then(|plugin| plugin.active_sha256);
        if after.as_deref() != Some(&before) {
            return Err(RuntimeError::PluginBusy);
        }
        if self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .contains(name)
        {
            return Err(RuntimeError::PluginPackageUnavailable);
        }
        Ok(PluginUseLease {
            name: name.to_owned(),
            sha256: before,
            _guard: guard,
        })
    }

    pub(crate) async fn config_lease(
        &self,
        name: &str,
        expected_sha256: &str,
    ) -> RuntimeResult<PluginConfigLease> {
        if !valid_name(name) || !valid_sha(expected_sha256) {
            return Err(invalid());
        }
        if self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .contains(name)
        {
            return Err(RuntimeError::PluginPackageUnavailable);
        }
        let guard = self
            .use_gate(name)
            .await
            .try_write_owned()
            .map_err(|_| RuntimeError::PluginBusy)?;
        let plugin = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|plugin| {
                plugin.name == name && plugin.active_sha256.as_deref() == Some(expected_sha256)
            })
            .ok_or(RuntimeError::PluginPackageUnavailable)?;
        if self
            .audit_state
            .lock()
            .expect("plugin audit poisoned")
            .corrupt_names
            .contains(name)
        {
            return Err(RuntimeError::PluginPackageUnavailable);
        }
        Ok(PluginConfigLease {
            name: plugin.name,
            sha256: expected_sha256.to_owned(),
            _guard: guard,
        })
    }

    async fn use_gate(&self, name: &str) -> Arc<RwLock<()>> {
        self.use_gates
            .lock()
            .await
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// Registry 刷新在安装写门持有期间跳过旧候选，避免再次启动待替换的包。
    pub(crate) async fn package_available_for_refresh(&self, name: &str) -> bool {
        self.use_gate(name).await.try_read().is_ok()
    }

    pub(crate) async fn begin_uninstall(
        self: &Arc<Self>,
        request: UninstallPluginRequest,
    ) -> RuntimeResult<PluginOperationSnapshot> {
        if !valid_name(&request.name) || !valid_sha(&request.expected_active_sha256) {
            return Err(invalid());
        }
        let _admission = self.admission.lock().await;
        let intent_json = serde_json::to_string(&request).map_err(|_| invalid())?;
        let existing = self
            .store
            .list_plugin_operations()
            .await
            .map_err(storage)?
            .iter()
            .any(|operation| operation.key == request.key);
        if !existing {
            if self.active.lock().await.is_some() {
                return Err(RuntimeError::PluginBusy);
            }
            self.ensure_no_unfinished_except(&request.key).await?;
            self.refresh_audit().await?;
        }
        let operation = self
            .accept(
                request.key.clone(),
                PluginOperationKind::Uninstall,
                intent_json,
            )
            .await?;
        if operation.phase == PluginOperationPhase::Accepted {
            self.spawn_uninstall(request.key.clone()).await?;
        }
        snapshot(&operation)
    }

    pub(crate) async fn cancel(
        &self,
        request: PluginOperationRequest,
    ) -> RuntimeResult<PluginOperationSnapshot> {
        let _commit = self.commit.lock().await;
        let mut operation = self.load(&request.key).await?;
        if matches!(
            operation.phase,
            PluginOperationPhase::Succeeded
                | PluginOperationPhase::Failed
                | PluginOperationPhase::Cancelled
        ) {
            return snapshot(&operation);
        }
        if !operation.cancel_requested {
            let previous = operation.phase;
            operation.cancel_requested = true;
            operation.updated_at_ms = now_ms();
            operation = self
                .advance(previous, operation, PluginRecordChange::Keep)
                .await?;
        }
        let active = self.active.lock().await;
        if let Some((key, token)) = &*active
            && key == &request.key
        {
            token.cancel();
        }
        let running = active.as_ref().is_some_and(|(key, _)| key == &request.key);
        drop(active);
        drop(_commit);
        if operation.phase == PluginOperationPhase::Accepted
            && !running
            && !self.uploads.lock().await.contains(&request.key)
        {
            self.stop_before_commit(&request.key, None, true).await?;
            return self.get(&request.key).await;
        }
        snapshot(&operation)
    }

    pub(crate) async fn retry_cleanup(
        self: &Arc<Self>,
        request: PluginOperationRequest,
    ) -> RuntimeResult<PluginOperationSnapshot> {
        let _admission = self.admission.lock().await;
        let operation = self.load(&request.key).await?;
        if operation.error_code.as_deref() != Some("cleanup_required") {
            return snapshot(&operation);
        }
        self.spawn_recovery(request.key.clone()).await?;
        snapshot(&operation)
    }

    pub(crate) async fn recover(self: &Arc<Self>) -> RuntimeResult<()> {
        let operations = self.store.list_plugin_operations().await.map_err(storage)?;
        self.refresh_audit().await?;
        for operation in operations {
            if matches!(
                operation.phase,
                PluginOperationPhase::Accepted
                    | PluginOperationPhase::Preparing
                    | PluginOperationPhase::Committing
                    | PluginOperationPhase::Cleaning
            ) {
                self.recover_one(operation).await?;
            }
        }
        self.refresh_audit().await?;
        Ok(())
    }

    /// 每次包操作结束后尝试一次历史残留清理；失败只保留诊断，不妨碍下一次操作。
    async fn recover_pending_cleanup(&self) -> RuntimeResult<()> {
        for operation in self.store.list_plugin_operations().await.map_err(storage)? {
            if operation.error_code.as_deref() == Some("cleanup_required")
                && matches!(
                    operation.phase,
                    PluginOperationPhase::Cleaning | PluginOperationPhase::Interrupted
                )
            {
                let _ = self.recover_one(operation).await;
            }
        }
        self.refresh_audit().await?;
        Ok(())
    }

    async fn refresh_audit(&self) -> RuntimeResult<PluginAudit> {
        let plugins = self.store.list_plugins().await.map_err(storage)?;
        let operations = self.store.list_plugin_operations().await.map_err(storage)?;
        let audit = self
            .files()?
            .audit(plugins.clone(), operations)
            .await
            .unwrap_or_else(|_| PluginAudit {
                corrupt_names: plugins
                    .iter()
                    .filter(|plugin| plugin.active_sha256.is_some())
                    .map(|plugin| plugin.name.clone())
                    .collect(),
                cleanup_blocked: true,
            });
        let changed = {
            let mut current = self.audit_state.lock().expect("plugin audit poisoned");
            if *current == audit {
                false
            } else {
                *current = audit.clone();
                true
            }
        };
        if changed {
            let _ = self.events.send(RuntimeEvent::PluginCatalogChanged);
        }
        Ok(audit)
    }

    async fn ensure_no_unfinished_except(&self, key: &IdempotencyKey) -> RuntimeResult<()> {
        for operation in self.store.list_plugin_operations().await.map_err(storage)? {
            if &operation.key != key
                && operation.error_code.as_deref() != Some("cleanup_required")
                && matches!(
                    operation.phase,
                    PluginOperationPhase::Accepted
                        | PluginOperationPhase::Preparing
                        | PluginOperationPhase::Committing
                        | PluginOperationPhase::Cleaning
                )
            {
                return Err(RuntimeError::PluginBusy);
            }
        }
        Ok(())
    }

    async fn accept(
        &self,
        key: IdempotencyKey,
        kind: PluginOperationKind,
        intent_json: String,
    ) -> RuntimeResult<StoredPluginOperation> {
        let time = now_ms();
        let operation = self
            .store
            .accept_plugin_operation(StoredPluginOperation {
                key,
                kind,
                intent_json,
                phase: PluginOperationPhase::Accepted,
                committed_json: "[]".into(),
                cancel_requested: false,
                error_code: None,
                created_at_ms: time,
                updated_at_ms: time,
            })
            .await
            .map_err(storage)?;
        let _ = self.events.send(RuntimeEvent::PluginOperationChanged {
            key: operation.key.clone(),
        });
        Ok(operation)
    }

    async fn load(&self, key: &IdempotencyKey) -> RuntimeResult<StoredPluginOperation> {
        self.store
            .list_plugin_operations()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|operation| &operation.key == key)
            .ok_or(RuntimeError::PluginOperationNotFound)
    }

    async fn advance(
        &self,
        expected: PluginOperationPhase,
        next: StoredPluginOperation,
        plugin: PluginRecordChange,
    ) -> RuntimeResult<StoredPluginOperation> {
        let changed_name = match &plugin {
            PluginRecordChange::Keep => None,
            PluginRecordChange::Upsert(plugin) => Some(plugin.name.clone()),
            PluginRecordChange::Remove(name) => Some(name.clone()),
        };
        let operation = self
            .store
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: expected,
                next,
                plugin,
            })
            .await
            .map_err(storage)?;
        if matches!(
            operation.phase,
            PluginOperationPhase::Succeeded
                | PluginOperationPhase::Failed
                | PluginOperationPhase::Cancelled
        ) {
            self.progress
                .lock()
                .expect("plugin progress poisoned")
                .remove(&operation.key);
            self.progress_notice
                .lock()
                .expect("plugin progress notice poisoned")
                .remove(&operation.key);
        }
        let _ = self.events.send(RuntimeEvent::PluginOperationChanged {
            key: operation.key.clone(),
        });
        if let Some(name) = changed_name {
            self.audit_state
                .lock()
                .expect("plugin audit poisoned")
                .corrupt_names
                .remove(&name);
            let _ = self.events.send(RuntimeEvent::PluginCatalogChanged);
        }
        Ok(operation)
    }

    async fn set_phase(
        &self,
        key: &IdempotencyKey,
        phase: PluginOperationPhase,
        error: Option<&str>,
    ) -> RuntimeResult<StoredPluginOperation> {
        let mut operation = self.load(key).await?;
        let previous = operation.phase;
        operation.phase = phase;
        operation.error_code = error.map(ToOwned::to_owned);
        operation.updated_at_ms = now_ms();
        self.advance(previous, operation, PluginRecordChange::Keep)
            .await
    }

    async fn spawn_install(self: &Arc<Self>, key: IdempotencyKey) -> RuntimeResult<()> {
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|(current, _)| current == &key) {
            return Ok(());
        }
        if active.is_some() {
            return Err(RuntimeError::PluginBusy);
        }
        let token = self.cancellation.child_token();
        *active = Some((key.clone(), token.clone()));
        let this = self.clone();
        let recovery = self.clone();
        let recovery_key = key.clone();
        self.tasks.spawn(
            async move {
                if this.run_install(&key, &token).await.is_err() {
                    let _ = this.recover_one_key(&key).await;
                }
                let _ = this.recover_pending_cleanup().await;
                let _ = this.events.send(RuntimeEvent::PluginCatalogChanged);
                this.active.lock().await.take();
            },
            async move {
                let _ = recovery.recover_one_key(&recovery_key).await;
                recovery.active.lock().await.take();
            },
        );
        Ok(())
    }

    async fn spawn_uninstall(self: &Arc<Self>, key: IdempotencyKey) -> RuntimeResult<()> {
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|(current, _)| current == &key) {
            return Ok(());
        }
        if active.is_some() {
            return Err(RuntimeError::PluginBusy);
        }
        let token = self.cancellation.child_token();
        *active = Some((key.clone(), token.clone()));
        let this = self.clone();
        let recovery = self.clone();
        let recovery_key = key.clone();
        self.tasks.spawn(
            async move {
                if this.run_uninstall(&key, &token).await.is_err() {
                    let _ = this.recover_one_key(&key).await;
                }
                let _ = this.recover_pending_cleanup().await;
                let _ = this.events.send(RuntimeEvent::PluginCatalogChanged);
                this.active.lock().await.take();
            },
            async move {
                let _ = recovery.recover_one_key(&recovery_key).await;
                recovery.active.lock().await.take();
            },
        );
        Ok(())
    }

    async fn spawn_recovery(self: &Arc<Self>, key: IdempotencyKey) -> RuntimeResult<()> {
        let mut active = self.active.lock().await;
        if active.is_some() {
            return Err(RuntimeError::PluginBusy);
        }
        *active = Some((key.clone(), self.cancellation.child_token()));
        let this = self.clone();
        let recovery = self.clone();
        self.tasks.spawn(
            async move {
                let _ = this.recover_one_key(&key).await;
                this.active.lock().await.take();
            },
            async move {
                recovery.active.lock().await.take();
            },
        );
        Ok(())
    }

    async fn run_install(
        &self,
        key: &IdempotencyKey,
        token: &CancellationToken,
    ) -> RuntimeResult<()> {
        let intent: BeginPluginInstallRequest =
            serde_json::from_str(&self.load(key).await?.intent_json).map_err(|_| invalid())?;
        let files = self.files()?;
        let mut committed = Vec::<CommittedPluginChange>::new();
        let package_count = intent.packages.len();
        for selection in intent.packages {
            if matches!(selection.origin, PluginPackageOrigin::Center { .. }) {
                self.report_progress(key, 0, selection.size_bytes);
            }
            if token.is_cancelled() || self.load(key).await?.cancel_requested {
                return self.stop_before_commit(key, Some(selection), true).await;
            }
            self.set_phase(key, PluginOperationPhase::Preparing, None)
                .await?;
            let progress = self.progress.clone();
            let notice = self.progress_notice.clone();
            let events = self.events.clone();
            let progress_key = key.clone();
            let total = selection.size_bytes;
            let report: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |received| {
                report_progress(&progress, &notice, &events, &progress_key, received, total);
            });
            let prepared = match files
                .prepare(key.clone(), selection.clone(), token.clone(), report)
                .await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self
                        .stop_before_commit_with_code(
                            key,
                            Some(selection),
                            matches!(error, PluginFileError::Cancelled),
                            plugin_file_code(&error),
                        )
                        .await;
                }
            };
            if prepared.sha256 != selection.sha256
                || !manifest_matches(&prepared.manifest_json, &selection)
            {
                return self
                    .stop_before_commit_with_code(key, Some(selection), false, "package_changed")
                    .await;
            }
            let old = self
                .store
                .list_plugins()
                .await
                .map_err(storage)?
                .into_iter()
                .find(|plugin| plugin.name == selection.name);
            if !matches_expected(&old, &selection) {
                return self
                    .stop_before_commit_with_code(key, Some(selection), false, "source_conflict")
                    .await;
            }
            if update_missing_required_config(old.as_ref(), &prepared.manifest_json) {
                return self
                    .stop_before_commit_with_code(
                        key,
                        Some(selection),
                        false,
                        "configuration_missing",
                    )
                    .await;
            }
            // 写门阻止刷新重新启动旧包；实际调用由 Registry 取消并等待关闭。
            let _use_guard = if old
                .as_ref()
                .is_some_and(|plugin| plugin.active_sha256.is_some())
            {
                let gate = self.use_gate(&selection.name).await;
                let guard = tokio::select! {
                    guard = gate.write_owned() => guard,
                    () = token.cancelled() => {
                        return self.stop_before_commit(key, Some(selection), true).await;
                    }
                };
                Some(guard)
            } else {
                None
            };
            if let Some(previous_sha256) = old
                .as_ref()
                .and_then(|plugin| plugin.active_sha256.as_deref())
                && self
                    .retire_old_mcp(&selection.name, previous_sha256)
                    .await
                    .is_err()
            {
                return self
                    .stop_before_commit_with_code(key, Some(selection), false, "mcp_close_failed")
                    .await;
            }
            if let Err(error) = files.publish(key.clone(), selection.clone()).await {
                return self
                    .stop_before_commit_with_code(
                        key,
                        Some(selection),
                        false,
                        plugin_file_code(&error),
                    )
                    .await;
            }
            let _commit = self.commit.lock().await;
            if token.is_cancelled() || self.load(key).await?.cancel_requested {
                return self.stop_before_commit(key, Some(selection), true).await;
            }
            self.set_phase(key, PluginOperationPhase::Committing, None)
                .await?;
            let previous_sha256 = old.as_ref().and_then(|plugin| plugin.active_sha256.clone());
            let change = CommittedPluginChange {
                name: selection.name.clone(),
                previous_sha256,
                next_sha256: Some(selection.sha256.clone()),
                clear_data: false,
            };
            committed.push(change.clone());
            let mut operation = self.load(key).await?;
            operation.phase = PluginOperationPhase::Cleaning;
            operation.committed_json = serde_json::to_string(&committed).map_err(|_| invalid())?;
            operation.updated_at_ms = now_ms();
            let plugin = StoredPlugin {
                name: selection.name.clone(),
                source: store_source(&selection.origin),
                active_version: Some(selection.version.clone()),
                active_sha256: Some(selection.sha256.clone()),
                active_manifest_json: Some(prepared.manifest_json),
                enabled: old.as_ref().is_none_or(|plugin| plugin.enabled),
                parameters_json: old
                    .as_ref()
                    .map_or_else(|| "{}".into(), |plugin| plugin.parameters_json.clone()),
                software_paths_json: old
                    .as_ref()
                    .map_or_else(|| "{}".into(), |plugin| plugin.software_paths_json.clone()),
                market_status: old.as_ref().and_then(|plugin| plugin.market_status.clone()),
                created_at_ms: old
                    .as_ref()
                    .map_or_else(now_ms, |plugin| plugin.created_at_ms),
                updated_at_ms: now_ms(),
            };
            self.advance(
                PluginOperationPhase::Committing,
                operation,
                PluginRecordChange::Upsert(plugin),
            )
            .await?;
            drop(_commit);
            if files.finish(key.clone(), change).await.is_err() {
                self.set_phase(
                    key,
                    PluginOperationPhase::Cleaning,
                    Some("cleanup_required"),
                )
                .await?;
                return Ok(());
            }
            if committed.len() < package_count {
                self.set_phase(key, PluginOperationPhase::Preparing, None)
                    .await?;
            }
        }
        self.set_phase(key, PluginOperationPhase::Succeeded, None)
            .await?;
        Ok(())
    }

    async fn run_uninstall(
        &self,
        key: &IdempotencyKey,
        token: &CancellationToken,
    ) -> RuntimeResult<()> {
        let request: UninstallPluginRequest =
            serde_json::from_str(&self.load(key).await?.intent_json).map_err(|_| invalid())?;
        self.set_phase(key, PluginOperationPhase::Preparing, None)
            .await?;
        let old = self
            .store
            .list_plugins()
            .await
            .map_err(storage)?
            .into_iter()
            .find(|plugin| plugin.name == request.name);
        let Some(old) = old.filter(|plugin| {
            plugin.active_sha256.as_deref() == Some(&request.expected_active_sha256)
        }) else {
            return self
                .stop_before_commit_with_code(key, None, false, "source_conflict")
                .await;
        };
        let _use_guard = match self.use_gate(&request.name).await.try_write_owned() {
            Ok(guard) => guard,
            Err(_) => {
                return self
                    .stop_before_commit_with_code(key, None, false, "busy")
                    .await;
            }
        };
        if self
            .retire_old_mcp(&request.name, &request.expected_active_sha256)
            .await
            .is_err()
        {
            return self
                .stop_before_commit_with_code(key, None, false, "busy")
                .await;
        }
        let _commit = self.commit.lock().await;
        if token.is_cancelled() || self.load(key).await?.cancel_requested {
            return self.stop_before_commit(key, None, true).await;
        }
        self.set_phase(key, PluginOperationPhase::Committing, None)
            .await?;
        let change = CommittedPluginChange {
            name: request.name.clone(),
            previous_sha256: old.active_sha256.clone(),
            next_sha256: None,
            clear_data: request.clear_data,
        };
        let mut operation = self.load(key).await?;
        operation.phase = PluginOperationPhase::Cleaning;
        operation.committed_json = serde_json::to_string(&[&change]).map_err(|_| invalid())?;
        operation.updated_at_ms = now_ms();
        let plugin_change = if request.clear_data {
            PluginRecordChange::Remove(request.name)
        } else {
            PluginRecordChange::Upsert(StoredPlugin {
                active_version: None,
                active_sha256: None,
                active_manifest_json: None,
                updated_at_ms: now_ms(),
                ..old
            })
        };
        self.advance(PluginOperationPhase::Committing, operation, plugin_change)
            .await?;
        drop(_commit);
        if self.files()?.finish(key.clone(), change).await.is_err() {
            self.set_phase(
                key,
                PluginOperationPhase::Cleaning,
                Some("cleanup_required"),
            )
            .await?;
            return Ok(());
        }
        self.set_phase(key, PluginOperationPhase::Succeeded, None)
            .await?;
        Ok(())
    }

    async fn stop_before_commit(
        &self,
        key: &IdempotencyKey,
        selection: Option<PluginPackageSelection>,
        cancelled: bool,
    ) -> RuntimeResult<()> {
        self.stop_before_commit_with_code(key, selection, cancelled, "package_invalid")
            .await
    }

    async fn stop_before_commit_with_code(
        &self,
        key: &IdempotencyKey,
        selection: Option<PluginPackageSelection>,
        cancelled: bool,
        code: &'static str,
    ) -> RuntimeResult<()> {
        if self.files()?.discard(key.clone(), selection).await.is_err() {
            self.set_phase(
                key,
                PluginOperationPhase::Interrupted,
                Some("cleanup_required"),
            )
            .await?;
        } else {
            self.set_phase(
                key,
                if cancelled {
                    PluginOperationPhase::Cancelled
                } else {
                    PluginOperationPhase::Failed
                },
                if cancelled {
                    None
                } else if code == "cleanup_required" {
                    Some("package_changed")
                } else {
                    Some(code)
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn recover_one_key(&self, key: &IdempotencyKey) -> RuntimeResult<()> {
        let operation = self.load(key).await?;
        self.recover_one(operation).await
    }

    async fn recover_one(&self, operation: StoredPluginOperation) -> RuntimeResult<()> {
        let files = self.files()?;
        match operation.phase {
            PluginOperationPhase::Accepted
            | PluginOperationPhase::Preparing
            | PluginOperationPhase::Committing
            | PluginOperationPhase::Interrupted => {
                let choice = if operation.kind == PluginOperationKind::Install {
                    serde_json::from_str::<BeginPluginInstallRequest>(&operation.intent_json)
                        .ok()
                        .and_then(|intent| {
                            intent
                                .packages
                                .into_iter()
                                .find(|choice| !committed_names(&operation).contains(&choice.name))
                        })
                } else {
                    None
                };
                let error = if files.discard(operation.key.clone(), choice).await.is_err() {
                    Some("cleanup_required")
                } else {
                    None
                };
                self.set_phase(&operation.key, PluginOperationPhase::Interrupted, error)
                    .await?;
            }
            PluginOperationPhase::Cleaning => {
                let Some(change) = committed_changes(&operation).last().cloned() else {
                    return Err(invalid());
                };
                if files.finish(operation.key.clone(), change).await.is_err() {
                    self.set_phase(
                        &operation.key,
                        PluginOperationPhase::Cleaning,
                        Some("cleanup_required"),
                    )
                    .await?;
                } else {
                    let complete = operation.kind == PluginOperationKind::Uninstall
                        || serde_json::from_str::<BeginPluginInstallRequest>(
                            &operation.intent_json,
                        )
                        .is_ok_and(|intent| {
                            committed_changes(&operation).len() == intent.packages.len()
                        });
                    self.set_phase(
                        &operation.key,
                        if complete {
                            PluginOperationPhase::Succeeded
                        } else {
                            PluginOperationPhase::Interrupted
                        },
                        None,
                    )
                    .await?;
                }
            }
            PluginOperationPhase::Succeeded
            | PluginOperationPhase::Failed
            | PluginOperationPhase::Cancelled => {}
        }
        Ok(())
    }
}

fn committed_changes(operation: &StoredPluginOperation) -> Vec<CommittedPluginChange> {
    serde_json::from_str(&operation.committed_json).unwrap_or_default()
}

fn settings_revision(plugin: &StoredPlugin) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"plugin-settings-v1\0");
    hasher.update(
        serde_json::to_vec(&(
            &plugin.active_sha256,
            plugin.enabled,
            &plugin.parameters_json,
            &plugin.software_paths_json,
        ))
        .expect("plugin settings tuple can be serialized"),
    );
    format!("sha256:{:x}", hasher.finalize())
}

fn declared_field(plugin: &StoredPlugin, section: &str, name: &str) -> bool {
    plugin
        .active_manifest_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|manifest| manifest.get(section)?.as_array().cloned())
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field.get("name").and_then(serde_json::Value::as_str) == Some(name))
        })
}

fn change_setting(document: &mut String, name: String, value: Option<String>) -> RuntimeResult<()> {
    if value
        .as_ref()
        .is_some_and(|value| value.len() > 32 * 1024 || value.contains('\0'))
    {
        return Err(invalid());
    }
    let mut values = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(document)
        .map_err(|_| invalid())?;
    if let Some(value) = value {
        values.insert(name, serde_json::Value::String(value));
    } else {
        values.remove(&name);
    }
    *document = serde_json::to_string(&values).map_err(|_| invalid())?;
    Ok(())
}

fn project_plugin(
    plugin: StoredPlugin,
    package_state: PluginPackageState,
) -> InstalledPluginSnapshot {
    let manifest = plugin
        .active_manifest_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok());
    let parameters =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&plugin.parameters_json)
            .unwrap_or_default();
    let software_paths = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
        &plugin.software_paths_json,
    )
    .unwrap_or_default();
    let configuration = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("configuration"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| {
            Some(PluginConfigurationFieldSnapshot {
                name: field.get("name")?.as_str()?.to_owned(),
                label: field.get("label")?.as_str()?.to_owned(),
                required: field.get("required")?.as_bool()?,
                secret: field.get("secret")?.as_bool()?,
                is_set: parameters
                    .get(field.get("name")?.as_str()?)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty()),
            })
        })
        .collect();
    let external_software = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("external_software"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| {
            Some(PluginSoftwareFieldSnapshot {
                name: field.get("name")?.as_str()?.to_owned(),
                display_name: field.get("display_name")?.as_str()?.to_owned(),
                is_set: software_paths
                    .get(field.get("name")?.as_str()?)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty()),
            })
        })
        .collect();
    InstalledPluginSnapshot {
        name: plugin.name.clone(),
        display_name: manifest
            .as_ref()
            .and_then(|manifest| manifest.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&plugin.name)
            .to_owned(),
        description: manifest
            .as_ref()
            .and_then(|manifest| manifest.get("description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        origin: origin(&plugin.source),
        version: plugin.active_version.clone(),
        sha256: plugin.active_sha256.clone(),
        enabled: plugin.enabled,
        package_state,
        settings_revision: settings_revision(&plugin),
        configuration,
        external_software,
        skills: declared_names(manifest.as_ref(), "skills"),
        mcp: declared_names(manifest.as_ref(), "mcp"),
    }
}

fn declared_names(manifest: Option<&serde_json::Value>, field: &str) -> Vec<String> {
    manifest
        .and_then(|manifest| manifest.get(field))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(ToOwned::to_owned))
        .collect()
}
fn report_progress(
    progress: &std::sync::Mutex<BTreeMap<IdempotencyKey, (u64, u64)>>,
    notice: &std::sync::Mutex<BTreeMap<IdempotencyKey, Instant>>,
    events: &ObservationCoordinator,
    key: &IdempotencyKey,
    received: u64,
    total: u64,
) {
    progress
        .lock()
        .expect("plugin progress poisoned")
        .insert(key.clone(), (received, total));
    let should_notify = {
        let mut noticed = notice.lock().expect("plugin progress notice poisoned");
        let now = Instant::now();
        if received == total
            || noticed
                .get(key)
                .is_none_or(|previous| now.duration_since(*previous) >= Duration::from_millis(250))
        {
            noticed.insert(key.clone(), now);
            true
        } else {
            false
        }
    };
    if should_notify {
        let _ = events.send(RuntimeEvent::PluginOperationChanged { key: key.clone() });
    }
}
fn committed_names(operation: &StoredPluginOperation) -> Vec<String> {
    committed_changes(operation)
        .into_iter()
        .map(|change| change.name)
        .collect()
}
fn snapshot(operation: &StoredPluginOperation) -> RuntimeResult<PluginOperationSnapshot> {
    Ok(PluginOperationSnapshot {
        key: operation.key.clone(),
        kind: match operation.kind {
            PluginOperationKind::Install => assistant_protocol::PluginOperationKind::Install,
            PluginOperationKind::Update => assistant_protocol::PluginOperationKind::Update,
            PluginOperationKind::Uninstall => assistant_protocol::PluginOperationKind::Uninstall,
        },
        phase: match operation.phase {
            PluginOperationPhase::Accepted => PluginOperationStatus::Accepted,
            PluginOperationPhase::Preparing => PluginOperationStatus::Preparing,
            PluginOperationPhase::Committing => PluginOperationStatus::Committing,
            PluginOperationPhase::Cleaning => PluginOperationStatus::Cleaning,
            PluginOperationPhase::Succeeded => PluginOperationStatus::Succeeded,
            PluginOperationPhase::Failed => PluginOperationStatus::Failed,
            PluginOperationPhase::Cancelled => PluginOperationStatus::Cancelled,
            PluginOperationPhase::Interrupted => PluginOperationStatus::Interrupted,
        },
        committed_names: committed_names(operation),
        cancel_requested: operation.cancel_requested,
        error_code: operation.error_code.clone(),
        cleanup_required: operation.error_code.as_deref() == Some("cleanup_required"),
        received_bytes: None,
        total_bytes: None,
    })
}
fn origin(source: &PluginSource) -> PluginPackageOrigin {
    match source {
        PluginSource::Upload => PluginPackageOrigin::Upload,
        PluginSource::Center(center_id) => PluginPackageOrigin::Center {
            center_id: center_id.clone(),
        },
    }
}
fn store_source(origin: &PluginPackageOrigin) -> PluginSource {
    match origin {
        PluginPackageOrigin::Upload => PluginSource::Upload,
        PluginPackageOrigin::Center { center_id } => PluginSource::Center(center_id.clone()),
    }
}
fn matches_expected(old: &Option<StoredPlugin>, selection: &PluginPackageSelection) -> bool {
    match old {
        None => selection.expected_active_sha256.is_none(),
        Some(plugin) => {
            origin(&plugin.source) == selection.origin
                && plugin.active_sha256 == selection.expected_active_sha256
        }
    }
}
fn manifest_matches(manifest: &str, selection: &PluginPackageSelection) -> bool {
    serde_json::from_str::<serde_json::Value>(manifest)
        .ok()
        .is_some_and(|value| {
            value.get("name").and_then(|value| value.as_str()) == Some(&selection.name)
                && value.get("version").and_then(|value| value.as_str()) == Some(&selection.version)
        })
}
fn update_missing_required_config(old: Option<&StoredPlugin>, manifest: &str) -> bool {
    let Some(old) = old.filter(|plugin| plugin.active_sha256.is_some()) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest) else {
        return true;
    };
    let Ok(parameters) = serde_json::from_str::<serde_json::Value>(&old.parameters_json) else {
        return true;
    };
    value
        .get("configuration")
        .and_then(|value| value.as_array())
        .is_some_and(|fields| {
            fields.iter().any(|field| {
                field.get("required").and_then(|value| value.as_bool()) == Some(true)
                    && field
                        .get("name")
                        .and_then(|value| value.as_str())
                        .is_some_and(|name| parameters.get(name).is_none())
            })
        })
}
fn validate_install(request: &BeginPluginInstallRequest) -> RuntimeResult<()> {
    if request.packages.is_empty() || request.packages.len() > 64 || request.packages.iter().any(|item| !valid_name(&item.name) || !valid_sha(&item.sha256) || item.expected_active_sha256.as_deref().is_some_and(|sha| !valid_sha(sha)) || item.version.is_empty() || item.version.len() > 80 || item.size_bytes == 0 || item.size_bytes > 2 * 1024 * 1024 * 1024 || matches!(&item.origin, PluginPackageOrigin::Center { center_id } if center_id.is_empty())) { return Err(invalid()); }
    if request
        .packages
        .iter()
        .any(|item| item.expected_active_sha256.as_deref() == Some(item.sha256.as_str()))
    {
        return Err(invalid());
    }
    if request
        .packages
        .iter()
        .filter(|item| matches!(item.origin, PluginPackageOrigin::Upload))
        .count()
        > 0
        && request.packages.len() != 1
    {
        return Err(invalid());
    }
    let mut names = std::collections::BTreeSet::new();
    if request
        .packages
        .iter()
        .any(|item| !names.insert(&item.name))
    {
        return Err(invalid());
    }
    Ok(())
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}
fn valid_sha(sha: &str) -> bool {
    sha.len() == 64
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn storage(source: crate::StoreError) -> RuntimeError {
    RuntimeError::StorageUnavailable {
        operation: "update plugin operation",
        source: Some(source),
    }
}
fn plugin_file_code(error: &PluginFileError) -> &'static str {
    match error {
        PluginFileError::Busy => "busy",
        PluginFileError::Invalid => "package_invalid",
        PluginFileError::Changed => "package_changed",
        PluginFileError::Cancelled => "cancelled",
        PluginFileError::Unavailable => "package_unavailable",
        PluginFileError::CleanupRequired => "cleanup_required",
    }
}
fn invalid() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "plugin operation is invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::VolatileRuntimeStore;
    use assistant_protocol::PluginArchiveFormat;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeFiles {
        prepared: AtomicUsize,
        published: AtomicUsize,
        discarded: AtomicUsize,
        finished: AtomicUsize,
        fail_prepare: AtomicBool,
        fail_prepare_at: AtomicUsize,
        wait_cancel: AtomicBool,
        fail_cleanup_once: AtomicBool,
        fail_cleanup: AtomicBool,
        fail_publish_conflict: AtomicBool,
        audit_corrupt: AtomicBool,
        audit_blocked: AtomicBool,
        audit_error: AtomicBool,
    }

    impl PluginFilePort for FakeFiles {
        fn prepare(
            &self,
            _: IdempotencyKey,
            choice: PluginPackageSelection,
            cancellation: CancellationToken,
            progress: Arc<dyn Fn(u64) + Send + Sync>,
        ) -> BoxFuture<'_, Result<PreparedPluginPackage, PluginFileError>> {
            Box::pin(async move {
                let attempt = self.prepared.fetch_add(1, Ordering::SeqCst) + 1;
                if self.wait_cancel.load(Ordering::SeqCst) {
                    cancellation.cancelled().await;
                    return Err(PluginFileError::Cancelled);
                }
                if self.fail_prepare.load(Ordering::SeqCst)
                    || self.fail_prepare_at.load(Ordering::SeqCst) == attempt
                {
                    return Err(PluginFileError::Invalid);
                }
                progress(choice.size_bytes);
                Ok(PreparedPluginPackage {
                    manifest_json: serde_json::json!({"name": choice.name, "version": choice.version, "configuration": []}).to_string(),
                    sha256: choice.sha256,
                })
            })
        }
        fn publish(
            &self,
            _: IdempotencyKey,
            _: PluginPackageSelection,
        ) -> BoxFuture<'_, Result<(), PluginFileError>> {
            Box::pin(async move {
                self.published.fetch_add(1, Ordering::SeqCst);
                if self.fail_publish_conflict.swap(false, Ordering::SeqCst) {
                    return Err(PluginFileError::CleanupRequired);
                }
                Ok(())
            })
        }
        fn discard(
            &self,
            _: IdempotencyKey,
            _: Option<PluginPackageSelection>,
        ) -> BoxFuture<'_, Result<(), PluginFileError>> {
            Box::pin(async move {
                self.discarded.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
        fn finish(
            &self,
            _: IdempotencyKey,
            _: CommittedPluginChange,
        ) -> BoxFuture<'_, Result<(), PluginFileError>> {
            Box::pin(async move {
                self.finished.fetch_add(1, Ordering::SeqCst);
                if self.fail_cleanup.load(Ordering::SeqCst)
                    || self.fail_cleanup_once.swap(false, Ordering::SeqCst)
                {
                    return Err(PluginFileError::CleanupRequired);
                }
                Ok(())
            })
        }
        fn audit(
            &self,
            plugins: Vec<StoredPlugin>,
            _: Vec<StoredPluginOperation>,
        ) -> BoxFuture<'_, Result<PluginAudit, PluginFileError>> {
            Box::pin(async move {
                if self.audit_error.load(Ordering::SeqCst) {
                    return Err(PluginFileError::Unavailable);
                }
                Ok(PluginAudit {
                    corrupt_names: if self.audit_corrupt.load(Ordering::SeqCst) {
                        plugins
                            .into_iter()
                            .filter(|plugin| plugin.active_sha256.is_some())
                            .map(|plugin| plugin.name)
                            .collect()
                    } else {
                        BTreeSet::new()
                    },
                    cleanup_blocked: self.audit_blocked.load(Ordering::SeqCst),
                })
            })
        }
    }

    fn manager(files: Arc<FakeFiles>) -> Arc<PluginManager> {
        let store: Arc<dyn RuntimeStore> = Arc::new(VolatileRuntimeStore::default());
        Arc::new(
            PluginManager::new(
                store,
                Arc::new(RuntimeTasks::new()),
                CancellationToken::new(),
                ObservationCoordinator::new(64),
            )
            .with_files(files),
        )
    }

    fn choice(name: &str, sha: char, expected: Option<String>) -> PluginPackageSelection {
        PluginPackageSelection {
            name: name.into(),
            version: "1.0.0".into(),
            sha256: sha.to_string().repeat(64),
            size_bytes: 32,
            format: PluginArchiveFormat::TarGzip,
            origin: PluginPackageOrigin::Center {
                center_id: "center-test".into(),
            },
            expected_active_sha256: expected,
        }
    }

    async fn settled(manager: &PluginManager, key: &IdempotencyKey) -> PluginOperationSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let operation = manager.get(key).await.unwrap();
                if (matches!(
                    operation.phase,
                    PluginOperationStatus::Succeeded
                        | PluginOperationStatus::Failed
                        | PluginOperationStatus::Cancelled
                        | PluginOperationStatus::Interrupted
                ) || operation.cleanup_required)
                    && manager.active.lock().await.is_none()
                {
                    return operation;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn install_is_idempotent_and_uninstall_retains_source_without_active_package() {
        let files = Arc::new(FakeFiles::default());
        let manager = manager(files.clone());
        let key = IdempotencyKey::new("install-a").unwrap();
        let request = BeginPluginInstallRequest {
            key: key.clone(),
            packages: vec![choice("document-tools", 'a', None)],
        };
        manager.begin_install(request.clone()).await.unwrap();
        assert_eq!(
            settled(&manager, &key).await.phase,
            PluginOperationStatus::Succeeded
        );
        assert_eq!(
            manager.begin_install(request).await.unwrap().phase,
            PluginOperationStatus::Succeeded
        );
        assert_eq!(files.prepared.load(Ordering::SeqCst), 1);
        let installed = manager.list().await.unwrap();
        assert_eq!(installed.plugins.len(), 1);
        assert_eq!(
            installed.plugins[0].sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        let uninstall = IdempotencyKey::new("uninstall-a").unwrap();
        manager
            .begin_uninstall(UninstallPluginRequest {
                key: uninstall.clone(),
                name: "document-tools".into(),
                expected_active_sha256: "a".repeat(64),
                clear_data: false,
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &uninstall).await.phase,
            PluginOperationStatus::Succeeded
        );
        let retained = manager.list().await.unwrap();
        assert_eq!(retained.plugins.len(), 1);
        assert!(retained.plugins[0].sha256.is_none());
    }

    #[tokio::test]
    async fn settings_use_content_cas_and_disable_new_package_leases() {
        let manager = manager(Arc::new(FakeFiles::default()));
        let key = IdempotencyKey::new("install-settings").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: key.clone(),
                packages: vec![choice("settings-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &key).await.phase,
            PluginOperationStatus::Succeeded
        );
        let installed = manager.list().await.unwrap().plugins.remove(0);
        let request = MutatePluginSettingsRequest {
            name: installed.name.clone(),
            expected_active_sha256: installed.sha256.clone().unwrap(),
            expected_revision: installed.settings_revision.clone(),
            mutation: PluginSettingsMutation::SetEnabled { enabled: false },
        };
        let changed = manager.mutate_settings(request.clone()).await.unwrap();
        assert!(!changed.plugin.enabled);
        assert!(manager.use_package(&installed.name).await.is_err());
        assert!(manager.mutate_settings(request).await.is_err());
    }

    #[tokio::test]
    async fn failed_update_keeps_previous_package_and_cleanup_does_not_block_other_operations() {
        let files = Arc::new(FakeFiles::default());
        let manager = manager(files.clone());
        let first = IdempotencyKey::new("first").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: first.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &first).await.phase,
            PluginOperationStatus::Succeeded
        );
        files.fail_prepare.store(true, Ordering::SeqCst);
        let second = IdempotencyKey::new("second").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: second.clone(),
                packages: vec![choice("document-tools", 'b', Some("a".repeat(64)))],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &second).await.phase,
            PluginOperationStatus::Failed
        );
        assert_eq!(
            manager.list().await.unwrap().plugins[0].sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        files.fail_prepare.store(false, Ordering::SeqCst);
        files.fail_cleanup.store(true, Ordering::SeqCst);
        let third = IdempotencyKey::new("third").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: third.clone(),
                packages: vec![choice("document-tools", 'b', Some("a".repeat(64)))],
            })
            .await
            .unwrap();
        assert!(settled(&manager, &third).await.cleanup_required);
        let fourth = IdempotencyKey::new("fourth").unwrap();
        for _ in 0..100 {
            match manager
                .begin_install(BeginPluginInstallRequest {
                    key: fourth.clone(),
                    packages: vec![choice("other-tools", 'c', None)],
                })
                .await
            {
                Ok(_) => break,
                Err(RuntimeError::PluginBusy) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected begin error: {error:?}"),
            }
        }
        assert!(settled(&manager, &fourth).await.cleanup_required);
        assert!(manager.get(&third).await.unwrap().cleanup_required);
        files.fail_cleanup.store(false, Ordering::SeqCst);
        let fifth = IdempotencyKey::new("fifth").unwrap();
        for _ in 0..100 {
            match manager
                .begin_install(BeginPluginInstallRequest {
                    key: fifth.clone(),
                    packages: vec![choice("third-tools", 'd', None)],
                })
                .await
            {
                Ok(_) => break,
                Err(RuntimeError::PluginBusy) => tokio::task::yield_now().await,
                Err(error) => panic!("unexpected begin error: {error:?}"),
            }
        }
        assert_eq!(
            settled(&manager, &fifth).await.phase,
            PluginOperationStatus::Succeeded
        );
        for _ in 0..100 {
            if !manager.get(&third).await.unwrap().cleanup_required {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            manager.get(&third).await.unwrap().phase,
            PluginOperationStatus::Succeeded
        );
        assert_eq!(
            manager.list().await.unwrap().plugins[0].sha256.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[tokio::test]
    async fn installing_the_current_package_again_does_not_create_an_operation() {
        let files = Arc::new(FakeFiles::default());
        let manager = manager(files.clone());
        let first = IdempotencyKey::new("install-current-first").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: first.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &first).await.phase,
            PluginOperationStatus::Succeeded
        );
        let result = manager
            .begin_install(BeginPluginInstallRequest {
                key: IdempotencyKey::new("install-current-again").unwrap(),
                packages: vec![choice("document-tools", 'a', Some("a".repeat(64)))],
            })
            .await;
        assert!(matches!(result, Err(RuntimeError::InvalidRequest { .. })));
        assert_eq!(manager.list().await.unwrap().operations.len(), 1);
        assert_eq!(files.published.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn publish_conflict_with_successful_discard_is_not_marked_for_cleanup() {
        let files = Arc::new(FakeFiles::default());
        files.fail_publish_conflict.store(true, Ordering::SeqCst);
        let manager = manager(files.clone());
        let failed = IdempotencyKey::new("publish-conflict").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: failed.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        let result = settled(&manager, &failed).await;
        assert_eq!(result.phase, PluginOperationStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("package_changed"));
        assert!(!result.cleanup_required);
        let next = IdempotencyKey::new("after-publish-conflict").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: next.clone(),
                packages: vec![choice("other-tools", 'b', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &next).await.phase,
            PluginOperationStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn cancellation_during_prepare_cleans_up_without_committing() {
        let files = Arc::new(FakeFiles::default());
        files.wait_cancel.store(true, Ordering::SeqCst);
        let manager = manager(files.clone());
        let key = IdempotencyKey::new("cancel-during-prepare").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: key.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while files.prepared.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let requested = manager
            .cancel(PluginOperationRequest { key: key.clone() })
            .await
            .unwrap();
        assert!(requested.cancel_requested);
        assert_eq!(
            settled(&manager, &key).await.phase,
            PluginOperationStatus::Cancelled
        );
        assert!(manager.list().await.unwrap().plugins.is_empty());
        assert_eq!(files.discarded.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn audit_failure_keeps_runtime_readable_and_other_package_changes_available() {
        let files = Arc::new(FakeFiles::default());
        let manager = manager(files.clone());
        let installed = IdempotencyKey::new("audit-installed").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: installed.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &installed).await.phase,
            PluginOperationStatus::Succeeded
        );
        files.audit_error.store(true, Ordering::SeqCst);
        manager.recover().await.unwrap();
        assert_eq!(
            manager
                .begin_install(BeginPluginInstallRequest {
                    key: installed.clone(),
                    packages: vec![choice("document-tools", 'a', None)],
                })
                .await
                .unwrap()
                .phase,
            PluginOperationStatus::Succeeded
        );
        let list = manager.list().await.unwrap();
        assert!(list.cleanup_blocked);
        assert_eq!(list.plugins[0].package_state, PluginPackageState::Corrupt);
        assert!(matches!(
            manager.use_package("document-tools").await,
            Err(RuntimeError::PluginPackageUnavailable)
        ));
        let next = IdempotencyKey::new("audit-next").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: next.clone(),
                packages: vec![choice("other-tools", 'b', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &next).await.phase,
            PluginOperationStatus::Succeeded
        );
        files.audit_error.store(false, Ordering::SeqCst);
        files.audit_corrupt.store(true, Ordering::SeqCst);
        manager.recover().await.unwrap();
        let list = manager.list().await.unwrap();
        assert!(!list.cleanup_blocked);
        assert_eq!(list.plugins[0].package_state, PluginPackageState::Corrupt);
        files.audit_corrupt.store(false, Ordering::SeqCst);
        files.audit_blocked.store(true, Ordering::SeqCst);
        manager.recover().await.unwrap();
        let list = manager.list().await.unwrap();
        assert!(list.cleanup_blocked);
        assert_eq!(list.plugins[0].package_state, PluginPackageState::Available);
    }

    #[tokio::test]
    async fn batch_failure_keeps_committed_first_package_and_stops_before_third() {
        let files = Arc::new(FakeFiles::default());
        files.fail_prepare_at.store(2, Ordering::SeqCst);
        let manager = manager(files.clone());
        let key = IdempotencyKey::new("batch-partial").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: key.clone(),
                packages: vec![
                    choice("first-tools", 'a', None),
                    choice("second-tools", 'b', None),
                    choice("third-tools", 'c', None),
                ],
            })
            .await
            .unwrap();
        let result = settled(&manager, &key).await;
        assert_eq!(result.phase, PluginOperationStatus::Failed);
        assert_eq!(result.committed_names, ["first-tools"]);
        assert_eq!(files.prepared.load(Ordering::SeqCst), 2);
        assert_eq!(files.finished.load(Ordering::SeqCst), 1);
        let plugins = manager.list().await.unwrap();
        assert_eq!(plugins.plugins.len(), 1);
        assert_eq!(plugins.plugins[0].name, "first-tools");
    }

    #[tokio::test]
    async fn active_use_makes_uninstall_busy_without_removing_package() {
        let files = Arc::new(FakeFiles::default());
        let manager = manager(files);
        let key = IdempotencyKey::new("lease-install").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: key.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &key).await.phase,
            PluginOperationStatus::Succeeded
        );
        let lease = manager.use_package("document-tools").await.unwrap();
        assert_eq!(lease.sha256, "a".repeat(64));
        let uninstall = IdempotencyKey::new("lease-uninstall").unwrap();
        manager
            .begin_uninstall(UninstallPluginRequest {
                key: uninstall.clone(),
                name: "document-tools".into(),
                expected_active_sha256: "a".repeat(64),
                clear_data: false,
            })
            .await
            .unwrap();
        let result = settled(&manager, &uninstall).await;
        assert_eq!(result.phase, PluginOperationStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("busy"));
        assert_eq!(
            manager.list().await.unwrap().plugins[0].sha256,
            Some("a".repeat(64))
        );
        drop(lease);
    }
    #[tokio::test]
    async fn update_waits_for_short_package_lease_instead_of_failing_busy() {
        let manager = manager(Arc::new(FakeFiles::default()));
        let first = IdempotencyKey::new("update-lease-first").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: first.clone(),
                packages: vec![choice("document-tools", 'a', None)],
            })
            .await
            .unwrap();
        assert_eq!(
            settled(&manager, &first).await.phase,
            PluginOperationStatus::Succeeded
        );
        let lease = manager.use_package("document-tools").await.unwrap();
        let update = IdempotencyKey::new("update-lease-next").unwrap();
        manager
            .begin_install(BeginPluginInstallRequest {
                key: update.clone(),
                packages: vec![choice("document-tools", 'b', Some("a".repeat(64)))],
            })
            .await
            .unwrap();
        // Tokio 写门排队后会阻止新读者；不靠 sleep 猜测更新是否到达写门。
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while manager
                .package_available_for_refresh("document-tools")
                .await
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(lease);
        let result = settled(&manager, &update).await;
        assert_eq!(result.phase, PluginOperationStatus::Succeeded);
        assert_eq!(result.error_code, None);
        assert_eq!(
            manager.list().await.unwrap().plugins[0].sha256,
            Some("b".repeat(64))
        );
    }
}
