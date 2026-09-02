//! Skill 设置页的显式扫描与全局名称开关命令。

use assistant_protocol::{
    GetSkillDetailRequest, GetSkillDetailResult, ListSkillsRequest, ListSkillsResult, RuntimeEvent,
    SetSkillEnabledRequest, SetSkillEnabledResult, SkillDetailSnapshot,
};

use super::{AssistantRuntime, now_ms, product::project_skill_management};
use crate::{
    RuntimeError, RuntimeResult, SkillDiagnostic, SkillDiagnosticCode, SkillDiscovery,
    SkillDiscoveryStatus, SkillName, SkillNameState, SkillNameStateChange, SkillScanRequest,
    StoredWorkspace, compile_skill_discovery,
};

impl AssistantRuntime {
    /// 每次调用都重新读取四个固定 Root，返回当前管理投影。
    pub async fn list_skills(&self, request: ListSkillsRequest) -> RuntimeResult<ListSkillsResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let workspace = self.skill_workspace(request.workspace_id.as_ref())?;
        let (discovery, states) = self.discover_skills(workspace.as_ref()).await?;
        Ok(ListSkillsResult {
            snapshot: project_skill_management(&discovery, &states),
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
        let management = project_skill_management(&discovery, &states);
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

    /// 按逻辑名称写入全局开关；已有 Session 的冻结 Catalog 和 Activation 不受影响。
    pub async fn set_skill_enabled(
        &self,
        request: SetSkillEnabledRequest,
    ) -> RuntimeResult<SetSkillEnabledResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let workspace = self.skill_workspace(request.workspace_id.as_ref())?;
        let name = SkillName::parse(request.name).map_err(|_| RuntimeError::SkillNameInvalid)?;
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
            snapshot: project_skill_management(&discovery, &states),
        })
    }

    fn skill_workspace(
        &self,
        workspace_id: Option<&assistant_protocol::WorkspaceId>,
    ) -> RuntimeResult<Option<StoredWorkspace>> {
        workspace_id
            .map(|workspace_id| self.workspace_for_new_session(workspace_id))
            .transpose()
    }

    async fn discover_skills(
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
                        std::iter::once(workspace.user_directory.clone())
                            .chain(workspace.additional_directories.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default(),
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
    let winner = discovery
        .winners
        .iter()
        .find(|candidate| &candidate.name == name)?;
    Some(winner.body.clone())
}
