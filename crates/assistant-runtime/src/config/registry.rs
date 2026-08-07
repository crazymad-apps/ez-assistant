//! 当前有效配置快照与串行 reload。

use std::sync::{Arc, RwLock};

use assistant_protocol::ModelKey;
use tokio::sync::Mutex;

use super::{
    compile_runtime_config,
    domain::{ConfigCompilation, ConfigProjection, ResolvedConfig, ResolvedModelConfig},
    source::{ConfigSourceLoad, RuntimeConfigSource},
};
use crate::{RuntimeError, RuntimeResult};

/// 一次原子可见的配置状态。
pub(crate) struct ConfigSnapshot {
    active: Option<Arc<ResolvedConfig>>,
    projection: Arc<ConfigProjection>,
}

impl ConfigSnapshot {
    fn from_compilation(compilation: ConfigCompilation) -> Self {
        Self {
            active: compilation.active.map(Arc::new),
            projection: Arc::new(compilation.projection),
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<ResolvedConfig>> {
        self.active.as_ref()
    }

    pub(crate) fn projection(&self) -> &ConfigProjection {
        &self.projection
    }

    /// 区分“用户 key 不存在”和“条目存在但当前无效”。
    pub(crate) fn contains_model_key(&self, key: &ModelKey) -> bool {
        self.projection
            .models
            .iter()
            .any(|model| model.model_key.as_ref() == Some(key))
    }

    pub(crate) fn model(&self, key: &ModelKey) -> Option<&ResolvedModelConfig> {
        self.active.as_ref()?.model(key)
    }
}

/// Runtime 的唯一配置状态所有者。
pub(crate) struct ConfigRegistry {
    source: Arc<dyn RuntimeConfigSource>,
    snapshot: RwLock<Arc<ConfigSnapshot>>,
    reload_gate: Mutex<()>,
}

impl ConfigRegistry {
    pub(crate) fn new(source: Arc<dyn RuntimeConfigSource>) -> Self {
        Self {
            source,
            snapshot: RwLock::new(Arc::new(ConfigSnapshot::from_compilation(
                ConfigCompilation::missing(),
            ))),
            reload_gate: Mutex::new(()),
        }
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
        let compilation = match self.source.load().await {
            ConfigSourceLoad::Missing => ConfigCompilation::missing(),
            ConfigSourceLoad::Document(document) => compile_runtime_config(&document),
            ConfigSourceLoad::Unavailable(failure) => {
                ConfigCompilation::source_unavailable(failure.kind(), failure.message())
            }
        };
        let next = Arc::new(ConfigSnapshot::from_compilation(compilation));
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
        let next = Arc::new(ConfigSnapshot::from_compilation(compile_runtime_config(
            document,
        )));
        *self.snapshot.write().expect("test registry lock") = next;
    }
}
