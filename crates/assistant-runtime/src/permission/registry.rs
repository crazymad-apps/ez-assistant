//! 权限文件的不可变编译快照与 cohort 原子替换。
//!
//! JSON 文件是用户可编辑的权威事实，Registry 只保存已通过校验的不可变编译视图。
//! `PermissionCoordinator` 串行化显式 reload 与审批产生的规则写入，避免二者用旧 revision
//! 相互覆盖；调用侧只短暂读取 `Arc` 快照，不跨 await 持有 Registry 锁。

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use assistant_protocol::{
    PermissionDiagnostic, PermissionDiagnosticCode, PermissionFileStatus, PermissionFileSummary,
};
use tokio::sync::Mutex;

use super::{
    PermissionDocument, PermissionFileLoad, PermissionFileRevision, PermissionFileScope,
    PermissionFileStore,
};
use crate::StoreErrorKind;
use crate::{RuntimeError, RuntimeResult};

#[derive(Clone)]
pub(crate) struct CompiledPermissionLoad {
    pub scope: PermissionFileScope,
    pub status: PermissionFileStatus,
    // M1 即保留快照对应的 revision，M3 持久授权将以它作为 CAS 前置。
    #[allow(dead_code)]
    pub revision: PermissionFileRevision,
    pub document: Option<Arc<PermissionDocument>>,
    pub diagnostics: Vec<PermissionDiagnostic>,
}

impl CompiledPermissionLoad {
    fn empty(scope: PermissionFileScope) -> Self {
        Self {
            scope,
            status: PermissionFileStatus::Empty,
            revision: PermissionFileRevision::Missing,
            document: Some(Arc::new(PermissionDocument::empty())),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn compile(scope: PermissionFileScope, load: PermissionFileLoad) -> Self {
        let level = scope.level();
        let mut diagnostics = load
            .diagnostics
            .into_iter()
            .map(|diagnostic| PermissionDiagnostic {
                scope: level,
                code: diagnostic.code,
                message: diagnostic.message.to_owned(),
            })
            .collect::<Vec<_>>();
        match load.content {
            None => Self {
                scope,
                status: PermissionFileStatus::Empty,
                revision: load.revision,
                document: Some(Arc::new(PermissionDocument::empty())),
                diagnostics,
            },
            Some(content) => match PermissionDocument::parse(&content) {
                Ok(document) => Self {
                    scope,
                    status: PermissionFileStatus::Ready,
                    revision: load.revision,
                    document: Some(Arc::new(document)),
                    diagnostics,
                },
                Err(error) => {
                    diagnostics.push(PermissionDiagnostic {
                        scope: level,
                        code: error.code(),
                        message: error.message().to_owned(),
                    });
                    Self {
                        scope,
                        status: PermissionFileStatus::Invalid,
                        revision: load.revision,
                        document: None,
                        diagnostics,
                    }
                }
            },
        }
    }

    pub(crate) fn summary(&self) -> PermissionFileSummary {
        PermissionFileSummary {
            scope: self.scope.level(),
            status: self.status,
        }
    }

    fn unavailable(scope: PermissionFileScope) -> Self {
        Self {
            diagnostics: vec![PermissionDiagnostic {
                scope: scope.level(),
                code: PermissionDiagnosticCode::Unavailable,
                message: "permission file could not be loaded".to_owned(),
            }],
            scope,
            status: PermissionFileStatus::Unavailable,
            revision: PermissionFileRevision::Missing,
            document: None,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.document.is_some()
    }
}

pub(crate) struct PermissionReloadOutcome {
    pub(crate) applied: bool,
    pub(crate) loads: Vec<CompiledPermissionLoad>,
}

/// 串行权限文件重载/写入并维护单一 Registry 的业务协调器。
pub(crate) struct PermissionCoordinator {
    store: Arc<dyn PermissionFileStore>,
    registry: PermissionRegistry,
    mutation_gate: Mutex<()>,
}

impl PermissionCoordinator {
    pub(crate) async fn open(
        store: Arc<dyn PermissionFileStore>,
        scopes: Vec<PermissionFileScope>,
    ) -> Self {
        let loads = load_scopes(store.as_ref(), scopes).await;
        Self {
            store,
            registry: PermissionRegistry::new(loads),
            mutation_gate: Mutex::new(()),
        }
    }

    pub(crate) fn empty(store: Arc<dyn PermissionFileStore>) -> Self {
        Self {
            store,
            registry: PermissionRegistry::empty(),
            mutation_gate: Mutex::new(()),
        }
    }

    pub(crate) async fn reload(
        &self,
        scopes: Vec<PermissionFileScope>,
    ) -> RuntimeResult<PermissionReloadOutcome> {
        let _gate = self.mutation_gate.lock().await;
        let loads = load_scopes(self.store.as_ref(), scopes).await;
        // cohort 要么整组替换，要么保持旧快照。不能让一次 reload 只更新三层规则中的
        // 一部分，否则同一次工具调用会观察到无法解释的混合版本。
        let applied = loads.iter().all(CompiledPermissionLoad::is_valid);
        if applied {
            self.registry.replace_cohort(loads.clone())?;
        }
        Ok(PermissionReloadOutcome { applied, loads })
    }

    pub(crate) fn register_empty_scope(&self, scope: PermissionFileScope) -> RuntimeResult<()> {
        self.registry
            .insert_if_absent(CompiledPermissionLoad::empty(scope))
    }

    pub(crate) async fn register_scope(&self, scope: PermissionFileScope) -> RuntimeResult<()> {
        let _gate = self.mutation_gate.lock().await;
        let load = match self.store.load_permission_file(&scope).await {
            Ok(load) => CompiledPermissionLoad::compile(scope, load),
            Err(_) => CompiledPermissionLoad::unavailable(scope),
        };
        self.registry.replace_scope(load)
    }

    /// 读取磁盘上的单份权限文档，不改变当前生效快照。
    pub(crate) async fn load_document(
        &self,
        scope: PermissionFileScope,
    ) -> RuntimeResult<CompiledPermissionLoad> {
        let _gate = self.mutation_gate.lock().await;
        let load = self
            .store
            .load_permission_file(&scope)
            .await
            .map_err(|_| RuntimeError::PermissionPersistenceFailed)?;
        Ok(CompiledPermissionLoad::compile(scope, load))
    }

    /// 校验完整 candidate，以 revision CAS 替换文件，并在成功后原子更新生效快照。
    pub(crate) async fn replace_document(
        &self,
        scope: PermissionFileScope,
        expected_revision: PermissionFileRevision,
        document: PermissionDocument,
    ) -> RuntimeResult<CompiledPermissionLoad> {
        let _gate = self.mutation_gate.lock().await;
        let content = document
            .render()
            .map_err(|_| RuntimeError::PermissionFileInvalid)?;
        let next_revision = self
            .store
            .replace_permission_file(&scope, &expected_revision, content.clone())
            .await
            .map_err(|error| match error.kind() {
                StoreErrorKind::Conflict => RuntimeError::PermissionFileConflict,
                _ => RuntimeError::PermissionPersistenceFailed,
            })?;
        let next = CompiledPermissionLoad::compile(
            scope,
            PermissionFileLoad {
                content: Some(content),
                revision: next_revision,
                diagnostics: Vec::new(),
            },
        );
        self.registry.replace_scope(next.clone())?;
        Ok(next)
    }

    /// 读取一次调用所需的完整 cohort。Registry 替换只持有短暂写锁，
    /// 因此活动 Run 的下一次工具调用会自然看到 reload 后的新快照。
    pub(crate) fn snapshot(
        &self,
        scopes: &[PermissionFileScope],
    ) -> RuntimeResult<Vec<Arc<CompiledPermissionLoad>>> {
        self.registry.snapshot_cohort(scopes)
    }

    /// 将交互审批产生的一条或多条 exact Allow 作为一次 CAS 变更可靠写入。
    pub(crate) async fn append_allow_rules(
        &self,
        scope: PermissionFileScope,
        rules: Vec<super::PermissionRule>,
    ) -> RuntimeResult<Vec<String>> {
        if rules.is_empty() {
            return Err(RuntimeError::PermissionPersistenceFailed);
        }
        let _gate = self.mutation_gate.lock().await;
        // 首次 CAS 冲突通常来自用户恰好保存了文件：重新读取、合并后只重试一次。
        // 持续冲突交还客户端处理，避免在用户编辑期间无限抢写。
        for attempt in 0..2 {
            match self
                .append_allow_rules_once(scope.clone(), rules.clone())
                .await
            {
                Err(RuntimeError::PermissionFileConflict) if attempt == 0 => continue,
                result => return result,
            }
        }
        unreachable!("two-attempt permission update loop always returns")
    }

    async fn append_allow_rules_once(
        &self,
        scope: PermissionFileScope,
        rules: Vec<super::PermissionRule>,
    ) -> RuntimeResult<Vec<String>> {
        // 每次 attempt 都从磁盘重新读取，而不是使用 Registry 快照作为写入基线。
        // Registry 可能是旧的运行视图，不能拿它覆盖用户刚刚手动编辑的 JSON。
        let load = self
            .store
            .load_permission_file(&scope)
            .await
            .map_err(|_| RuntimeError::PermissionPersistenceFailed)?;
        let revision = load.revision.clone();
        let compiled = CompiledPermissionLoad::compile(scope.clone(), load);
        let Some(document) = &compiled.document else {
            return Err(RuntimeError::PermissionFileInvalid);
        };
        let mut document = document.as_ref().clone();
        let previous_rule_count = document.rules.len();
        let rule_ids = rules
            .into_iter()
            .map(|rule| {
                document
                    .append_rule(rule)
                    .map_err(|_| RuntimeError::PermissionFileInvalid)
            })
            .collect::<RuntimeResult<Vec<_>>>()?;
        // 相同精确规则已存在时直接复用；不做无意义 CAS，也不重排用户文件。
        if document.rules.len() == previous_rule_count {
            // 当前磁盘文件可能由用户刚刚编辑，仍需让 Registry 看见这份最新有效内容。
            self.registry.replace_cohort(vec![compiled])?;
            return Ok(rule_ids);
        }
        let content = document
            .render()
            .map_err(|_| RuntimeError::PermissionFileInvalid)?;
        let next_revision = self
            .store
            .replace_permission_file(&scope, &revision, content.clone())
            .await
            .map_err(|error| match error.kind() {
                StoreErrorKind::Conflict => RuntimeError::PermissionFileConflict,
                _ => RuntimeError::PermissionPersistenceFailed,
            })?;
        // replace 成功后再用实际写入内容建立新快照。此顺序保证方法返回时，后续工具调用
        // 既能在磁盘恢复该授权，也能立即在当前进程匹配该授权。
        let next = CompiledPermissionLoad::compile(
            scope,
            PermissionFileLoad {
                content: Some(content),
                revision: next_revision,
                diagnostics: Vec::new(),
            },
        );
        self.registry.replace_cohort(vec![next])?;
        Ok(rule_ids)
    }

    #[cfg(test)]
    pub(crate) fn registry(&self) -> &PermissionRegistry {
        &self.registry
    }
}

async fn load_scopes(
    store: &dyn PermissionFileStore,
    scopes: Vec<PermissionFileScope>,
) -> Vec<CompiledPermissionLoad> {
    let mut loads = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let load = match store.load_permission_file(&scope).await {
            Ok(load) => CompiledPermissionLoad::compile(scope, load),
            Err(_) => CompiledPermissionLoad::unavailable(scope),
        };
        loads.push(load);
    }
    loads
}

/// JSON 是唯一权威源；此 Registry 只是已编译文件的不可变视图。
pub(crate) struct PermissionRegistry {
    snapshots: RwLock<BTreeMap<PermissionFileScope, Arc<CompiledPermissionLoad>>>,
}

impl PermissionRegistry {
    pub(crate) fn new(loads: Vec<CompiledPermissionLoad>) -> Self {
        Self {
            snapshots: RwLock::new(
                loads
                    .into_iter()
                    .map(|load| (load.scope.clone(), Arc::new(load)))
                    .collect(),
            ),
        }
    }

    pub(crate) fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// 只在 cohort 全部有效时原子替换，避免三层规则半新半旧。
    pub(crate) fn replace_cohort(&self, loads: Vec<CompiledPermissionLoad>) -> RuntimeResult<()> {
        if loads.iter().any(|load| !load.is_valid()) {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "permission registry invalid cohort",
            });
        }
        let mut snapshots =
            self.snapshots
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "permission registry",
                })?;
        for load in loads {
            snapshots.insert(load.scope.clone(), Arc::new(load));
        }
        Ok(())
    }

    fn insert_if_absent(&self, load: CompiledPermissionLoad) -> RuntimeResult<()> {
        let mut snapshots =
            self.snapshots
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "permission registry",
                })?;
        snapshots
            .entry(load.scope.clone())
            .or_insert_with(|| Arc::new(load));
        Ok(())
    }

    fn replace_scope(&self, load: CompiledPermissionLoad) -> RuntimeResult<()> {
        let mut snapshots =
            self.snapshots
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "permission registry",
                })?;
        snapshots.insert(load.scope.clone(), Arc::new(load));
        Ok(())
    }

    fn snapshot_cohort(
        &self,
        scopes: &[PermissionFileScope],
    ) -> RuntimeResult<Vec<Arc<CompiledPermissionLoad>>> {
        let snapshots =
            self.snapshots
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "permission registry",
                })?;
        scopes
            .iter()
            .map(|scope| {
                snapshots
                    .get(scope)
                    .cloned()
                    .ok_or(RuntimeError::InternalStateUnavailable {
                        component: "permission registry scope",
                    })
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn snapshot(
        &self,
        scope: &PermissionFileScope,
    ) -> RuntimeResult<Option<Arc<CompiledPermissionLoad>>> {
        self.snapshots
            .read()
            .map(|snapshots| snapshots.get(scope).cloned())
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "permission registry",
            })
    }
}
