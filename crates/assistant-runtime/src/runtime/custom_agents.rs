//! 个人自定义智能体配置；来源目录与首次发送在同一 Runtime 业务边界扩展。

use std::{collections::BTreeMap, sync::Arc};

use assistant_protocol::{
    AgentCapabilityReference, AgentDependencySnapshot, AgentDependencyStatus,
    CustomAgentListSnapshot, CustomAgentSelection, CustomAgentSnapshot, CustomAgentSource,
    DeletePersonalAgentRequest, DeletedPersonalAgentResult, PluginPackageOrigin,
    PluginPackageState, PrepareCustomAgentRequest, PreparedCustomAgentSnapshot, RuntimeEvent,
    SavePersonalAgentRequest,
};
use futures_util::future::{BoxFuture, FutureExt, Shared};

use crate::CenterCustomAgentDirectory;

type SharedRefresh = Shared<BoxFuture<'static, Result<Arc<CenterCustomAgentDirectory>, ()>>>;

#[derive(Default)]
pub(super) struct DirectoryState {
    next_generation: u64,
    in_flight: Option<(u64, SharedRefresh)>,
    last_valid: Option<Arc<CenterCustomAgentDirectory>>,
}

use super::AssistantRuntime;
use crate::{
    CustomAgentMutation, RuntimeError, RuntimeResult, StoreErrorKind, StoredCustomAgent,
    custom_agent_revision, valid_custom_agent_config, valid_custom_agent_name,
};

#[derive(Default)]
struct RequiredPluginCapabilities {
    skills: Vec<String>,
    mcp: Vec<String>,
}

struct DependencyFacts {
    ready: bool,
    wrong_origin: bool,
    capability_disabled: bool,
    catalog_unavailable: bool,
    declared: bool,
    name_occupied: bool,
}

impl AssistantRuntime {
    pub(super) async fn resolve_custom_agent(
        &self,
        selection: &CustomAgentSelection,
    ) -> RuntimeResult<CustomAgentSnapshot> {
        if !valid_custom_agent_name(&selection.name) {
            return Err(invalid());
        }
        match &selection.source {
            CustomAgentSource::Personal => self
                .store
                .list_custom_agents()
                .await
                .map_err(|source| RuntimeError::from_store("list custom agents", source))?
                .into_iter()
                .find(|item| item.config.name == selection.name)
                .map(project_personal)
                .ok_or(RuntimeError::ConfigurationUnavailable),
            CustomAgentSource::Center { center_id } => {
                let directory = self.refresh_center_custom_agents().await?;
                if &directory.center_id != center_id {
                    return Err(RuntimeError::ConfigurationUnavailable);
                }
                directory
                    .agents
                    .iter()
                    .find(|item| item.name == selection.name)
                    .cloned()
                    .map(|config| project_center(center_id, config))
                    .ok_or(RuntimeError::ConfigurationUnavailable)
            }
        }
    }

    pub async fn prepare_custom_agent(
        &self,
        request: PrepareCustomAgentRequest,
    ) -> RuntimeResult<PreparedCustomAgentSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let agent = self.resolve_custom_agent(&request.selection).await?;
        self.prepare_resolved_custom_agent(&agent, request.workspace_id.as_ref())
            .await
    }

    pub(super) async fn prepare_resolved_custom_agent(
        &self,
        agent: &CustomAgentSnapshot,
        workspace_id: Option<&assistant_protocol::WorkspaceId>,
    ) -> RuntimeResult<PreparedCustomAgentSnapshot> {
        let workspace = workspace_id
            .map(|id| self.workspace_for_new_session(id))
            .transpose()?;
        let (skills, skill_states) = self.discover_skills(workspace.as_ref()).await?;
        let mcp = self.mcp_service.registry.catalog_snapshot()?;
        let plugins = self.plugin.list().await?.plugins;
        let mut required = BTreeMap::<String, RequiredPluginCapabilities>::new();
        let mut dependencies = Vec::new();
        for reference in &agent.config.skills {
            if let Some(name) = &reference.plugin {
                required
                    .entry(name.clone())
                    .or_default()
                    .skills
                    .push(reference.name.clone());
            }
            let installed = reference
                .plugin
                .as_deref()
                .and_then(|name| plugins.iter().find(|item| item.name == name));
            let wrong_origin =
                installed.is_some_and(|item| !plugin_matches_source(item, &agent.source));
            let plugin = installed.filter(|item| plugin_matches_source(item, &agent.source));
            let winner = skills
                .winners
                .iter()
                .find(|item| item.name.as_str() == reference.name);
            let ready = winner.is_some_and(|item| {
                item.plugin_name.as_deref() == reference.plugin.as_deref()
                    && plugin.is_none_or(|package| {
                        item.plugin_sha256.as_deref() == package.sha256.as_deref()
                    })
            });
            let declared =
                plugin.is_some_and(|item| item.skills.iter().any(|name| name == &reference.name));
            dependencies.push(AgentDependencySnapshot {
                kind: "skill".into(),
                reference: reference.clone(),
                status: dependency_status(
                    reference,
                    plugin,
                    DependencyFacts {
                        ready,
                        wrong_origin,
                        capability_disabled: skill_states
                            .iter()
                            .any(|state| state.name.as_str() == reference.name && !state.enabled),
                        catalog_unavailable: skills.status
                            == crate::SkillDiscoveryStatus::Unavailable,
                        declared,
                        name_occupied: skills
                            .candidates
                            .iter()
                            .any(|item| item.name.as_str() == reference.name),
                    },
                ),
            });
        }
        for reference in &agent.config.mcp {
            if let Some(name) = &reference.plugin {
                required
                    .entry(name.clone())
                    .or_default()
                    .mcp
                    .push(reference.name.clone());
            }
            let installed = reference
                .plugin
                .as_deref()
                .and_then(|name| plugins.iter().find(|item| item.name == name));
            let wrong_origin =
                installed.is_some_and(|item| !plugin_matches_source(item, &agent.source));
            let plugin = installed.filter(|item| plugin_matches_source(item, &agent.source));
            let winner = mcp
                .iter()
                .find(|item| item.server_key.as_str() == reference.name);
            let ready = winner.is_some_and(|item| {
                item.plugin_name.as_deref() == reference.plugin.as_deref()
                    && plugin.is_none_or(|package| {
                        item.plugin_sha256.as_deref() == package.sha256.as_deref()
                    })
            });
            let declared =
                plugin.is_some_and(|item| item.mcp.iter().any(|name| name == &reference.name));
            dependencies.push(AgentDependencySnapshot {
                kind: "mcp".into(),
                reference: reference.clone(),
                status: dependency_status(
                    reference,
                    plugin,
                    DependencyFacts {
                        ready,
                        wrong_origin,
                        capability_disabled: false,
                        catalog_unavailable: false,
                        declared,
                        name_occupied: winner.is_some(),
                    },
                ),
            });
        }
        let mut install_candidates = Vec::new();
        if let CustomAgentSource::Center { center_id } = &agent.source {
            let source = self
                .custom_agent_source
                .as_ref()
                .ok_or(RuntimeError::ConfigurationUnavailable)?;
            for (name, requirements) in required {
                let missing = dependencies.iter().any(|item| {
                    item.reference.plugin.as_deref() == Some(&name)
                        && item.status == AgentDependencyStatus::PluginMissing
                });
                if !missing {
                    continue;
                }
                let selection = match source
                    .resolve_plugin(center_id, &name, requirements.skills, requirements.mcp)
                    .await
                {
                    Ok(selection) => selection,
                    Err(_) => {
                        for item in &mut dependencies {
                            if item.reference.plugin.as_deref() == Some(&name)
                                && item.status == AgentDependencyStatus::PluginMissing
                            {
                                item.status = AgentDependencyStatus::RuntimeUnavailable;
                            }
                        }
                        continue;
                    }
                };
                if selection.name != name
                    || selection.origin
                        != (PluginPackageOrigin::Center {
                            center_id: center_id.clone(),
                        })
                {
                    return Err(RuntimeError::ConfigurationUnavailable);
                }
                install_candidates.push(selection);
            }
        }
        Ok(PreparedCustomAgentSnapshot {
            selection: CustomAgentSelection {
                name: agent.config.name.clone(),
                source: agent.source.clone(),
            },
            revision: agent.revision.clone(),
            dependencies,
            install_candidates,
        })
    }

    pub async fn list_custom_agents(&self) -> RuntimeResult<CustomAgentListSnapshot> {
        self.ensure_running()?;
        let mut agents = self
            .store
            .list_custom_agents()
            .await
            .map_err(|source| RuntimeError::from_store("list custom agents", source))?
            .into_iter()
            .map(project_personal)
            .collect::<Vec<_>>();
        let center_unavailable = if self.custom_agent_source.is_some() {
            let refreshed = self.refresh_center_custom_agents().await;
            let cached = self.custom_agent_directory.lock().await.last_valid.clone();
            if let Some(cached) = cached.filter(|directory| {
                self.custom_agent_source
                    .as_ref()
                    .is_some_and(|source| source.can_use_cached(&directory.center_id))
            }) {
                agents.extend(
                    cached
                        .agents
                        .iter()
                        .cloned()
                        .map(|config| project_center(&cached.center_id, config)),
                );
            }
            refreshed.is_err()
        } else {
            false
        };
        agents.sort_by(|left, right| {
            left.config
                .name
                .cmp(&right.config.name)
                .then_with(|| format!("{:?}", left.source).cmp(&format!("{:?}", right.source)))
        });
        Ok(CustomAgentListSnapshot {
            agents,
            center_unavailable,
        })
    }

    pub(super) async fn refresh_center_custom_agents(
        &self,
    ) -> RuntimeResult<Arc<CenterCustomAgentDirectory>> {
        let source = self
            .custom_agent_source
            .as_ref()
            .ok_or(RuntimeError::ConfigurationUnavailable)?
            .clone();
        let (generation, refresh) = {
            let mut state = self.custom_agent_directory.lock().await;
            if let Some((generation, refresh)) = &state.in_flight {
                (*generation, refresh.clone())
            } else {
                state.next_generation = state.next_generation.saturating_add(1);
                let generation = state.next_generation;
                let refresh = async move { source.load().await.map(Arc::new).map_err(|_| ()) }
                    .boxed()
                    .shared();
                state.in_flight = Some((generation, refresh.clone()));
                (generation, refresh)
            }
        };
        let result = refresh.await;
        let mut state = self.custom_agent_directory.lock().await;
        if state
            .in_flight
            .as_ref()
            .is_some_and(|(current, _)| *current == generation)
        {
            state.in_flight = None;
            if let Ok(directory) = &result
                && !directory.center_id.is_empty()
                && directory.agents.iter().all(valid_custom_agent_config)
                && unique_names(&directory.agents)
            {
                state.last_valid = Some(directory.clone());
                let _ = self
                    .event_sender
                    .send(RuntimeEvent::CustomAgentCatalogChanged);
            }
        }
        match result {
            Ok(directory)
                if !directory.center_id.is_empty()
                    && directory.agents.iter().all(valid_custom_agent_config)
                    && unique_names(&directory.agents) =>
            {
                Ok(directory)
            }
            _ => Err(RuntimeError::ConfigurationUnavailable),
        }
    }

    pub async fn save_personal_agent(
        &self,
        request: SavePersonalAgentRequest,
    ) -> RuntimeResult<CustomAgentSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if !valid_custom_agent_config(&request.config) {
            return Err(invalid());
        }
        let current = self
            .store
            .list_custom_agents()
            .await
            .map_err(|source| RuntimeError::from_store("list custom agents", source))?
            .into_iter()
            .find(|agent| agent.config.name == request.config.name);
        if current
            .as_ref()
            .map(|agent| custom_agent_revision(&agent.config))
            != request.expected_revision
        {
            return Err(RuntimeError::SnapshotStale);
        }
        let now = u64::try_from(super::now_ms()?).map_err(|_| invalid())?;
        let next = StoredCustomAgent {
            config: request.config,
            created_at_ms: current.as_ref().map_or(now, |agent| agent.created_at_ms),
            updated_at_ms: current
                .as_ref()
                .map_or(now, |agent| now.max(agent.updated_at_ms.saturating_add(1))),
        };
        let saved = self
            .store
            .mutate_custom_agent(CustomAgentMutation::Save {
                expected_revision: request.expected_revision,
                next,
            })
            .await
            .map_err(|source| match source.kind() {
                StoreErrorKind::Conflict => RuntimeError::SnapshotStale,
                _ => RuntimeError::from_store("save custom agent", source),
            })?
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "saved custom agent",
            })?;
        let _ = self
            .event_sender
            .send(RuntimeEvent::CustomAgentCatalogChanged);
        Ok(project_personal(saved))
    }

    pub async fn delete_personal_agent(
        &self,
        request: DeletePersonalAgentRequest,
    ) -> RuntimeResult<DeletedPersonalAgentResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        if !valid_custom_agent_name(&request.name) {
            return Err(invalid());
        }
        self.store
            .mutate_custom_agent(CustomAgentMutation::Delete {
                name: request.name.clone(),
                expected_revision: request.expected_revision,
            })
            .await
            .map_err(|source| match source.kind() {
                StoreErrorKind::Conflict => RuntimeError::SnapshotStale,
                _ => RuntimeError::from_store("delete custom agent", source),
            })?;
        let _ = self
            .event_sender
            .send(RuntimeEvent::CustomAgentCatalogChanged);
        Ok(DeletedPersonalAgentResult { name: request.name })
    }
}

fn dependency_status(
    reference: &AgentCapabilityReference,
    plugin: Option<&assistant_protocol::InstalledPluginSnapshot>,
    facts: DependencyFacts,
) -> AgentDependencyStatus {
    if facts.wrong_origin {
        return AgentDependencyStatus::NameConflict;
    }
    if reference.plugin.is_some() && plugin.is_none_or(|item| item.sha256.is_none()) {
        return AgentDependencyStatus::PluginMissing;
    }
    if let Some(plugin) = plugin {
        if !plugin.enabled {
            return AgentDependencyStatus::PluginDisabled;
        }
        if plugin.package_state != PluginPackageState::Available {
            return AgentDependencyStatus::RuntimeUnavailable;
        }
        if plugin
            .configuration
            .iter()
            .any(|field| field.required && !field.is_set)
        {
            return AgentDependencyStatus::ConfigurationMissing;
        }
        if plugin.external_software.iter().any(|field| !field.is_set) {
            return AgentDependencyStatus::SoftwareMissing;
        }
    }
    if facts.capability_disabled {
        return AgentDependencyStatus::CapabilityDisabled;
    }
    if facts.catalog_unavailable {
        return AgentDependencyStatus::RuntimeUnavailable;
    }
    if facts.ready {
        AgentDependencyStatus::Available
    } else if facts.name_occupied {
        AgentDependencyStatus::NameConflict
    } else if facts.declared {
        AgentDependencyStatus::RuntimeUnavailable
    } else {
        AgentDependencyStatus::CapabilityMissing
    }
}

fn plugin_matches_source(
    plugin: &assistant_protocol::InstalledPluginSnapshot,
    source: &CustomAgentSource,
) -> bool {
    match source {
        CustomAgentSource::Personal => true,
        CustomAgentSource::Center { center_id } => {
            plugin.origin
                == (PluginPackageOrigin::Center {
                    center_id: center_id.clone(),
                })
        }
    }
}

fn project_personal(agent: StoredCustomAgent) -> CustomAgentSnapshot {
    CustomAgentSnapshot {
        source: CustomAgentSource::Personal,
        revision: custom_agent_revision(&agent.config),
        config: agent.config,
        created_at_ms: Some(agent.created_at_ms),
        updated_at_ms: Some(agent.updated_at_ms),
    }
}

fn project_center(
    center_id: &str,
    config: assistant_protocol::CustomAgentConfig,
) -> CustomAgentSnapshot {
    CustomAgentSnapshot {
        source: CustomAgentSource::Center {
            center_id: center_id.to_owned(),
        },
        revision: custom_agent_revision(&config),
        config,
        created_at_ms: None,
        updated_at_ms: None,
    }
}

fn unique_names(agents: &[assistant_protocol::CustomAgentConfig]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    agents.iter().all(|agent| seen.insert(agent.name.as_str()))
}

fn invalid() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "custom agent config is invalid",
    }
}

/// 环境与能力故障只作诊断；缺包、显式禁用和来源冲突仍要求先处理，不能隐式安装或启用。
pub(super) fn dependency_blocks_session(status: AgentDependencyStatus) -> bool {
    match status {
        AgentDependencyStatus::PluginMissing
        | AgentDependencyStatus::PluginDisabled
        | AgentDependencyStatus::CapabilityDisabled
        | AgentDependencyStatus::NameConflict => true,
        AgentDependencyStatus::Available
        | AgentDependencyStatus::CapabilityMissing
        | AgentDependencyStatus::ConfigurationMissing
        | AgentDependencyStatus::SoftwareMissing
        | AgentDependencyStatus::RuntimeUnavailable => false,
    }
}

pub(super) fn with_agent_prompt(
    prompt: agent_model::SystemPromptSnapshot,
    agent: Option<&CustomAgentSnapshot>,
) -> agent_model::SystemPromptSnapshot {
    let Some(agent) = agent else { return prompt };
    let mut parts = prompt.into_parts();
    let directory = parts.pop();
    parts.push(agent.config.prompt.clone());
    if !agent.config.skills.is_empty() || !agent.config.mcp.is_empty() {
        let skills = agent
            .config
            .skills
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mcp = agent
            .config
            .mcp
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Custom agent capability hints (use when relevant; not an allowlist): skills [{skills}]; MCP servers [{mcp}]. Availability is not guaranteed. Missing software, configuration or capabilities do not prevent working on the task. Inspect the current environment and use available tools to diagnose, resolve or work around limitations, subject to existing permissions and approvals. Only invoke tools actually exposed in the current tool set."
        ));
    }
    if let Some(directory) = directory {
        parts.push(directory);
    }
    agent_model::SystemPromptSnapshot::new(parts)
}
