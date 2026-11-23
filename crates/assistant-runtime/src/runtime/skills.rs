//! Skill 设置页的显式扫描与全局名称开关命令。

use assistant_protocol::{
    GetSkillDetailRequest, GetSkillDetailResult, ListSkillsRequest, ListSkillsResult, RuntimeEvent,
    SetSkillEnabledRequest, SetSkillEnabledResult, SkillDetailSnapshot,
};

use super::{
    AssistantRuntime, now_ms,
    product::{project_active_skills, project_skill_management},
};
use crate::{
    RuntimeError, RuntimeResult, SessionExecutionEnvironment, SkillDiagnostic, SkillDiagnosticCode,
    SkillDiscovery, SkillDiscoveryStatus, SkillName, SkillNameState, SkillNameStateChange,
    SkillScanRequest, StoredWorkspace, compile_skill_discovery,
};

impl AssistantRuntime {
    /// 每次调用都重新读取四个固定 Root，返回当前管理投影。
    pub async fn list_skills(&self, request: ListSkillsRequest) -> RuntimeResult<ListSkillsResult> {
        let contextual = request.session_id.is_some() || request.page.is_some();
        let window = contextual
            .then(|| super::context_page::PageWindow::new(request.page))
            .transpose()?;
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = match &request.session_id {
            Some(id) => {
                let session = self.session(id).await?;
                session
                    .ensure_conversation_loaded(self.store.as_ref())
                    .await?;
                Some(session)
            }
            None => None,
        };
        let _mutation = self.workspace_mutation_gate.lock().await;
        let workspace_id = session
            .as_ref()
            .map_or(request.workspace_id.as_ref(), |session| {
                session.environment().workspace_id.as_ref()
            });
        let workspace = self.skill_workspace(workspace_id)?;
        let (discovery, states) = self.discover_skills(workspace.as_ref()).await?;
        let mut snapshot = project_skill_management(
            &discovery,
            &states,
            request.include_plugins.unwrap_or(false),
        );
        let Some(window) = window else {
            return Ok(ListSkillsResult {
                snapshot,
                page: None,
                diagnostic_count: None,
                active_skills: Vec::new(),
            });
        };
        let mut active_skills = match session {
            Some(session) => project_active_skills(&*session.lock_state()?)?,
            None => Vec::new(),
        };
        let active_names = active_skills
            .iter()
            .map(|skill| skill.tag.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        snapshot.skills.retain(|skill| {
            skill.enabled && skill.health == assistant_protocol::SkillHealthSnapshot::Ready
        });
        snapshot
            .skills
            .sort_by_key(|skill| !active_names.contains(skill.name.as_str()));
        let (skills, page) = window.take(snapshot.skills.into_iter());
        active_skills.retain(|active| skills.iter().any(|skill| skill.name == active.tag.name));
        snapshot.skills = skills;
        let diagnostic_count = u32::try_from(snapshot.diagnostics.len()).unwrap_or(u32::MAX);
        snapshot.diagnostics.clear();
        Ok(ListSkillsResult {
            snapshot,
            page: Some(page),
            diagnostic_count: Some(diagnostic_count),
            active_skills,
        })
    }

    /// 每次调用都重新扫描指定范围，只把目标名称的当前正文投影给设置详情页。
    pub async fn get_skill_detail(
        &self,
        request: GetSkillDetailRequest,
    ) -> RuntimeResult<GetSkillDetailResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let workspace = self.skill_workspace(request.workspace_id.as_ref())?;
        let name = SkillName::parse(request.name).map_err(|_| RuntimeError::SkillNameInvalid)?;
        let (discovery, states) = self.discover_skills(workspace.as_ref()).await?;
        let management = project_skill_management(&discovery, &states, false);
        let detail = management
            .skills
            .iter()
            .find(|skill| skill.name == name.as_str())
            .cloned()
            .map(|skill| SkillDetailSnapshot {
                body: current_skill_body(&discovery, &name),
                diagnostics: management
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.skill_name.as_deref() == Some(name.as_str()))
                    .cloned()
                    .collect(),
                skill,
            });
        Ok(GetSkillDetailResult { detail })
    }

    /// 按逻辑名称写入全局开关；当前查询与后续激活立即使用新开关；已冻结系统提示词和 Activation 不受影响。
    pub async fn set_skill_enabled(
        &self,
        request: SetSkillEnabledRequest,
    ) -> RuntimeResult<SetSkillEnabledResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let workspace = self.skill_workspace(request.workspace_id.as_ref())?;
        let name = SkillName::parse(request.name).map_err(|_| RuntimeError::SkillNameInvalid)?;
        let (before, _) = self.discover_skills(workspace.as_ref()).await?;
        if before.candidates.iter().any(|candidate| {
            candidate.name == name && candidate.source == crate::SkillSource::Plugin
        }) && !before.candidates.iter().any(|candidate| {
            candidate.name == name && candidate.source != crate::SkillSource::Plugin
        }) {
            return Err(RuntimeError::InvalidRequest {
                reason: "plugin skill cannot be changed through independent skill settings",
            });
        }
        let changed_at_ms =
            u64::try_from(now_ms()?).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "system clock range",
            })?;
        self.store
            .set_skill_enabled(SkillNameStateChange {
                name: name.clone(),
                enabled: request.enabled,
                updated_at_ms: changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("set skill enabled", source))?;
        let (discovery, states) = self.discover_skills(workspace.as_ref()).await?;
        self.publish(RuntimeEvent::SkillSettingsChanged {
            name: name.as_str().to_owned(),
            enabled: request.enabled,
        });
        Ok(SetSkillEnabledResult {
            snapshot: project_skill_management(&discovery, &states, false),
        })
    }

    /// 新激活和执行装配使用 Session 冻结根；Skill 文件本身每次重新扫描。
    pub(super) async fn current_skill_catalog(
        &self,
        session: &crate::session::SessionController,
    ) -> RuntimeResult<crate::SkillCatalog> {
        prepare_current_catalog(
            self.store.as_ref(),
            self.skill_package_source.as_ref(),
            self.plugin.as_ref(),
            session.environment(),
        )
        .await
    }

    fn skill_workspace(
        &self,
        workspace_id: Option<&assistant_protocol::WorkspaceId>,
    ) -> RuntimeResult<Option<StoredWorkspace>> {
        workspace_id
            .map(|workspace_id| self.workspace_for_new_session(workspace_id))
            .transpose()
    }

    pub(super) async fn discover_skills(
        &self,
        workspace: Option<&StoredWorkspace>,
    ) -> RuntimeResult<(SkillDiscovery, Vec<SkillNameState>)> {
        let states = self
            .store
            .list_skill_name_states()
            .await
            .map_err(|source| RuntimeError::from_store("load skill name states", source))?;
        let scan = match self
            .skill_package_source
            .scan(SkillScanRequest {
                workspace_directories: workspace
                    .map(|workspace| {
                        std::iter::once(workspace.effective_working_directory().to_owned())
                            .chain(workspace.additional_directories.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default(),
                plugin_packages: self.plugin.active_skill_packages().await?,
            })
            .await
        {
            Ok(scan) => scan,
            Err(_) => {
                return Ok((
                    SkillDiscovery {
                        status: SkillDiscoveryStatus::Unavailable,
                        candidates: Vec::new(),
                        winners: Vec::new(),
                        diagnostics: vec![SkillDiagnostic::error(
                            SkillDiagnosticCode::ScanIncomplete,
                            "skill package scan did not complete",
                        )],
                    },
                    states,
                ));
            }
        };
        Ok((compile_skill_discovery(scan, &states), states))
    }
}

/// 只有扫描完整且最高优先级候选唯一时，详情页才展示该候选正文。
fn current_skill_body(discovery: &SkillDiscovery, name: &SkillName) -> Option<String> {
    if discovery.status != SkillDiscoveryStatus::Available {
        return None;
    }
    let winner = discovery.winners.iter().find(|candidate| {
        &candidate.name == name && candidate.source != crate::SkillSource::Plugin
    })?;
    Some(winner.body.clone())
}

/// 创建与显式刷新共用有界发现流程；扫描失败不返回部分目录。
pub(super) async fn prepare_catalog(
    store: &dyn crate::RuntimeStore,
    source: &dyn crate::SkillPackageSource,
    plugin: &crate::plugin::PluginManager,
    workspace_directories: Vec<String>,
) -> RuntimeResult<crate::SkillCatalog> {
    let states = store
        .list_skill_name_states()
        .await
        .map_err(|source| RuntimeError::from_store("load skill name states", source))?;
    let scan = match source
        .scan(crate::SkillScanRequest {
            workspace_directories,
            plugin_packages: plugin.active_skill_packages().await?,
        })
        .await
    {
        Ok(scan) => scan,
        Err(_) => {
            return Ok(crate::SkillCatalog::unavailable(vec![
                crate::SkillDiagnostic::error(
                    crate::SkillDiagnosticCode::ScanIncomplete,
                    "skill package scan did not complete",
                ),
            ]));
        }
    };
    let discovery = crate::compile_skill_discovery(scan, &states);
    if discovery.status == crate::SkillDiscoveryStatus::Unavailable {
        return Ok(crate::SkillCatalog::unavailable(discovery.diagnostics));
    }
    crate::SkillCatalog::from_discovery(discovery).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "skill catalog",
        }
    })
}

/// 已有 Session 只从冻结根重新扫描 Skill，不随 Workspace 后续目录编辑换根。
pub(super) async fn prepare_current_catalog(
    store: &dyn crate::RuntimeStore,
    source: &dyn crate::SkillPackageSource,
    plugin: &crate::plugin::PluginManager,
    environment: &SessionExecutionEnvironment,
) -> RuntimeResult<crate::SkillCatalog> {
    let directories = if environment.workspace_id.is_some() {
        std::iter::once(environment.working_directory.clone())
            .chain(environment.additional_workspace_directories.iter().cloned())
            .collect()
    } else {
        Vec::new()
    };
    prepare_catalog(store, source, plugin, directories).await
}

/// Run 持有其 Skill Catalog 实际引用的包，扫描与取得租约之间的变更会重试。
pub(super) async fn prepare_run_catalog(
    store: &dyn crate::RuntimeStore,
    source: &dyn crate::SkillPackageSource,
    plugin: &crate::plugin::PluginManager,
    environment: &SessionExecutionEnvironment,
) -> RuntimeResult<(crate::SkillCatalog, Vec<crate::PluginUseLease>)> {
    for _ in 0..3 {
        let catalog = prepare_current_catalog(store, source, plugin, environment).await?;
        let packages = catalog
            .definitions
            .iter()
            .filter_map(|definition| {
                Some((
                    definition.plugin_name.as_ref()?,
                    definition.plugin_sha256.as_ref()?,
                ))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut leases = Vec::with_capacity(packages.len());
        let mut stale = false;
        for (name, sha256) in packages {
            match plugin.use_package(name).await {
                Ok(lease) if &lease.sha256 == sha256 => leases.push(lease),
                Ok(_) | Err(RuntimeError::PluginBusy | RuntimeError::PluginPackageUnavailable) => {
                    stale = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if !stale {
            return Ok((catalog, leases));
        }
    }
    Err(RuntimeError::PluginBusy)
}
