//! 当前有效配置快照与串行 reload。

use std::sync::{Arc, RwLock};

use assistant_protocol::{ModelConfigurationInput, ModelKey};
use tokio::sync::Mutex;

use super::{
    ConfigMutation, compile_runtime_config,
    domain::{ConfigCompilation, ConfigProjection, ResolvedConfig, ResolvedModelConfig},
    edit_config_document,
    source::{ConfigSourceLoad, ConfigSourceReplace, RuntimeConfigSource},
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
                None,
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

    /// 在唯一配置源上执行一次 revision CAS 变更，并仅在 candidate 可编译时交换快照。
    pub(crate) async fn mutate(
        &self,
        expected_revision: Option<String>,
        mutation: ConfigMutation,
    ) -> RuntimeResult<Arc<ConfigSnapshot>> {
        let _gate = self.reload_gate.lock().await;
        let current = self.source.load().await;
        let (current_contents, observed_revision) = match &current {
            ConfigSourceLoad::Missing => (None, None),
            ConfigSourceLoad::Document(document) => {
                (Some(document.contents()), Some(document.revision()))
            }
            ConfigSourceLoad::Unavailable(_) => {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
        };
        if observed_revision != expected_revision.as_deref() {
            self.install_load(current)?;
            return Err(RuntimeError::ConfigurationConflict);
        }

        let target_model_key = mutation.target_model_key().clone();
        let requires_valid_target = mutation.requires_valid_target();
        let candidate = edit_config_document(current_contents, mutation)?;
        let compilation = compile_runtime_config(&candidate);
        let target_is_valid = compilation
            .projection()
            .models
            .iter()
            .any(|model| model.model_key.as_ref() == Some(&target_model_key) && model.is_valid);
        if compilation.active().is_none() || (requires_valid_target && !target_is_valid) {
            return Err(RuntimeError::InvalidRequest {
                reason: "model configuration candidate is invalid",
            });
        }

        match self.source.replace(expected_revision, candidate).await {
            ConfigSourceReplace::Applied(document) => {
                let compilation = compile_runtime_config(document.contents());
                if compilation.active().is_none() {
                    return Err(RuntimeError::ConfigurationPersistenceFailed);
                }
                self.install(compilation, Some(document.revision().to_owned()))
            }
            ConfigSourceReplace::Conflict(latest) => {
                self.install_load(latest)?;
                Err(RuntimeError::ConfigurationConflict)
            }
            ConfigSourceReplace::Unavailable(_) => {
                Err(RuntimeError::ConfigurationPersistenceFailed)
            }
        }
    }

    /// 编译一个尚未持久化的表单 candidate，供显式连接测试使用。
    pub(crate) async fn candidate_snapshot(
        &self,
        model: ModelConfigurationInput,
    ) -> RuntimeResult<Arc<ConfigSnapshot>> {
        let _gate = self.reload_gate.lock().await;
        let load = self.source.load().await;
        let current = match &load {
            ConfigSourceLoad::Missing => None,
            ConfigSourceLoad::Document(document) => Some(document.contents()),
            ConfigSourceLoad::Unavailable(_) => {
                return Err(RuntimeError::ConfigurationUnavailable);
            }
        };
        let exists = current.is_some_and(|document| {
            compile_runtime_config(document)
                .projection()
                .models
                .iter()
                .any(|candidate| candidate.model_key.as_ref() == Some(&model.model_key))
        });
        let candidate = edit_config_document(
            current,
            if exists {
                ConfigMutation::Update {
                    model,
                    set_default: false,
                }
            } else {
                ConfigMutation::Create {
                    model,
                    set_default: true,
                }
            },
        )?;
        Ok(Arc::new(ConfigSnapshot::from_compilation(
            compile_runtime_config(&candidate),
            None,
        )))
    }

    fn install_load(&self, load: ConfigSourceLoad) -> RuntimeResult<Arc<ConfigSnapshot>> {
        let (compilation, revision) = match load {
            ConfigSourceLoad::Missing => (ConfigCompilation::missing(), None),
            ConfigSourceLoad::Document(document) => (
                compile_runtime_config(document.contents()),
                Some(document.revision().to_owned()),
            ),
            ConfigSourceLoad::Unavailable(failure) => (
                ConfigCompilation::source_unavailable(failure.kind(), failure.message()),
                None,
            ),
        };
        self.install(compilation, revision)
    }

    fn install(
        &self,
        compilation: ConfigCompilation,
        revision: Option<String>,
    ) -> RuntimeResult<Arc<ConfigSnapshot>> {
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
