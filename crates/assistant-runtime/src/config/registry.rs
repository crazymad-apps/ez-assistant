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
#[derive(Clone)]
pub(crate) struct ConfigSnapshot {
    active: Option<Arc<ResolvedConfig>>,
    projection: Arc<ConfigProjection>,
    revision: Option<String>,
    // 无效快照仍禁止模型调用；只保留非敏感采样值用于下次接纳比较，不保留旧服务。
    last_generation: Option<agent_model::GenerationConfig>,
}

impl ConfigSnapshot {
    fn from_compilation(compilation: ConfigCompilation, revision: Option<String>) -> Self {
        Self {
            last_generation: compilation
                .active
                .as_ref()
                .map(|active| active.generation().clone()),
            active: compilation.active.map(Arc::new),
            projection: Arc::new(compilation.projection),
            revision,
        }
    }

    pub(crate) fn active(&self) -> Option<&Arc<ResolvedConfig>> {
        self.active.as_ref()
    }

    pub(crate) fn accepted_generation(&self) -> Option<&agent_model::GenerationConfig> {
        self.last_generation.as_ref()
    }

    pub(crate) fn projection(&self) -> &ConfigProjection {
        &self.projection
    }

    pub(crate) fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }
}

/// 数据库模型设置与 Provider（含最后成功目录）的内存投影；不建立第二份目录缓存或加载全量固定记录。
#[derive(Default)]
pub(crate) struct ManagedModels {
    pub(crate) providers:
        BTreeMap<assistant_protocol::ProviderInstanceId, Arc<crate::StoredProvider>>,
    pub(crate) settings: assistant_protocol::ModelSettings,
    pub(crate) source: super::ModelSource,
    pub(crate) external: Option<Arc<super::ExternalModelConfiguration>>,
}

/// Runtime 的唯一配置状态所有者。
pub(crate) struct ConfigRegistry {
    source: Arc<dyn RuntimeConfigSource>,
    snapshot: RwLock<Arc<ConfigSnapshot>>,
    reload_gate: Mutex<()>,
    model_settings: RwLock<ManagedModels>,
}

impl ConfigRegistry {
    pub(crate) fn new(
        source: Arc<dyn RuntimeConfigSource>,
        model_source: super::ModelSource,
    ) -> Self {
        // 启动占位缺省值尚未由配置源接纳；首次加载不得使重启恢复的续接周期换代。
        let mut initial = ConfigSnapshot::from_compilation(ConfigCompilation::missing(), None);
        initial.last_generation = None;
        Self {
            source,
            snapshot: RwLock::new(Arc::new(initial)),
            reload_gate: Mutex::new(()),
            model_settings: RwLock::new(ManagedModels {
                source: model_source,
                ..Default::default()
            }),
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

    /// 串行读取，完成宿主接纳动作后才发布快照；不在 Registry 内访问 Store。
    pub(crate) async fn reload<F, Fut>(
        &self,
        events: &crate::observation::ObservationCoordinator,
        before_publish: F,
    ) -> RuntimeResult<Arc<ConfigSnapshot>>
    where
        F: FnOnce(&ConfigSnapshot, &ConfigSnapshot) -> Fut,
        Fut: std::future::Future<Output = RuntimeResult<()>>,
    {
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
        let previous = self.snapshot()?;
        let mut next = ConfigSnapshot::from_compilation(compilation, revision);
        if next.last_generation.is_none() {
            next.last_generation = previous.last_generation.clone();
        }
        let next = Arc::new(next);
        before_publish(&previous, &next).await?;
        events.publish_transition(|| {
            *self
                .snapshot
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "configuration registry",
                })? = next.clone();
            Ok(((), Some(assistant_protocol::RuntimeEvent::ConfigChanged)))
        })?;
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
