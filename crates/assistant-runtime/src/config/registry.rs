//! 当前有效配置快照与串行 reload。

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use tokio::sync::Mutex;

use super::{
    compile_runtime_config,
    domain::{ConfigCompilation, ConfigProjection, ResolvedConfig},
    source::{ConfigSourceLoad, RuntimeConfigSource},
};
use crate::{RuntimeError, RuntimeResult};

/// 一次原子可见的配置状态。
pub(crate) struct ConfigSnapshot {
    active: Option<Arc<ResolvedConfig>>,
    projection: Arc<ConfigProjection>,
    revision: Option<String>,
}

impl ConfigSnapshot {
    fn from_compilation(compilation: ConfigCompilation, revision: Option<String>) -> Self {
        Self {
            active: compilation.active.map(Arc::new),
            projection: Arc::new(compilation.projection),
            revision,
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<ResolvedConfig>> {
        self.active.as_ref()
    }

    pub(crate) fn projection(&self) -> &ConfigProjection {
        &self.projection
    }

    pub(crate) fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }
}

/// 数据库模型设置，不保存在线目录或全量固定记录。Provider Arc 身份涵盖连接／固定设置变更。
#[derive(Default)]
pub(crate) struct ManagedModels {
    pub(crate) providers:
        BTreeMap<assistant_protocol::ProviderInstanceId, Arc<crate::StoredProvider>>,
    pub(crate) settings: assistant_protocol::ModelSettings,
}

/// Runtime 的唯一配置状态所有者。
pub(crate) struct ConfigRegistry {
    source: Arc<dyn RuntimeConfigSource>,
    snapshot: RwLock<Arc<ConfigSnapshot>>,
    reload_gate: Mutex<()>,
    model_settings: RwLock<ManagedModels>,
}

impl ConfigRegistry {
    pub(crate) fn new(source: Arc<dyn RuntimeConfigSource>) -> Self {
        Self {
            source,
            snapshot: RwLock::new(Arc::new(ConfigSnapshot::from_compilation(
                ConfigCompilation::missing(),
                None,
            ))),
            reload_gate: Mutex::new(()),
            model_settings: RwLock::new(ManagedModels::default()),
        }
    }

    pub(crate) fn managed_models(&self) -> RuntimeResult<RwLockReadGuard<'_, ManagedModels>> {
        self.model_settings
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "model settings",
            })
    }
    pub(crate) fn managed_models_mut(&self) -> RuntimeResult<RwLockWriteGuard<'_, ManagedModels>> {
        self.model_settings
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "model settings",
            })
    }

    pub(crate) fn snapshot(&self) -> RuntimeResult<Arc<ConfigSnapshot>> {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "configuration registry",
            })
    }

    pub(crate) fn display_path(&self) -> Option<String> {
        self.source.display_path()
    }

    /// 串行读取并原子替换整个快照；失败结果不会回退到旧 credential。
    pub(crate) async fn reload(&self) -> RuntimeResult<Arc<ConfigSnapshot>> {
        let _gate = self.reload_gate.lock().await;
        let (compilation, revision) = match self.source.load().await {
            ConfigSourceLoad::Missing => (ConfigCompilation::missing(), None),
            ConfigSourceLoad::Document(document) => {
                let compilation = compile_runtime_config(document.contents());
                (compilation, Some(document.revision().to_owned()))
            }
            ConfigSourceLoad::Unavailable(failure) => (
                ConfigCompilation::source_unavailable(failure.kind(), failure.message()),
                None,
            ),
        };
        let next = Arc::new(ConfigSnapshot::from_compilation(compilation, revision));
        *self
            .snapshot
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "configuration registry",
            })? = next.clone();
        Ok(next)
    }

    #[cfg(test)]
    pub(crate) fn replace_document_for_test(&self, document: &str) {
        let next = Arc::new(ConfigSnapshot::from_compilation(
            compile_runtime_config(document),
            Some("test-revision".to_owned()),
        ));
        *self.snapshot.write().expect("test registry lock") = next;
    }
}
