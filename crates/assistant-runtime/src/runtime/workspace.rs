//! Workspace Registry 的业务命令与 Session 绑定查询。

use assistant_protocol::{
    GetWorkspaceRequest, GetWorkspaceResult, ListWorkspacesRequest, ListWorkspacesResult,
    RegisterWorkspaceRequest, RegisterWorkspaceResult, RemoveWorkspaceRequest,
    RemoveWorkspaceResult, UpdateWorkspaceRequest, UpdateWorkspaceResult, WorkspaceId,
};

use super::AssistantRuntime;
use crate::{
    NewWorkspaceRegistration, RuntimeError, RuntimeResult, StoreErrorKind, StoredWorkspace,
    StoredWorkspaceLifecycle, WorkspaceRemoval, WorkspaceUpdate, id,
    permission::PermissionFileScope, workspace::summary,
};

const MAX_WORKSPACE_LABEL_CHARS: usize = 80;
const MAX_WORKSPACE_DIRECTORIES: usize = 16;

impl AssistantRuntime {
    /// 按 canonical path 幂等登记 Workspace；重新登记已移除路径会恢复原 ID。
    pub async fn register_workspace(
        &self,
        request: RegisterWorkspaceRequest,
    ) -> RuntimeResult<RegisterWorkspaceResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let (label, primary_directory, additional_directories) = validate_workspace_form(
            request.label,
            request.primary_directory,
            request.additional_directories,
        )?;
        let workspace_id = self.allocate_workspace_id()?;
        let stored = self
            .store
            .register_workspace(NewWorkspaceRegistration {
                workspace_id: workspace_id.clone(),
                label,
                requested_primary_directory: primary_directory,
                requested_additional_directories: additional_directories,
                changed_at_ms: super::now_ms()?,
            })
            .await
            .map_err(|source| {
                if source.kind() == StoreErrorKind::ResourceUnavailable {
                    RuntimeError::WorkspaceUnavailable {
                        workspace_id: workspace_id.clone(),
                    }
                } else {
                    RuntimeError::from_store("register workspace", source)
                }
            })?;
        let restored = self
            .workspaces
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .get(&stored.workspace_id)
            .is_some_and(|workspace| workspace.lifecycle == StoredWorkspaceLifecycle::Removed);
        self.permission_coordinator
            .register_scope(PermissionFileScope::Workspace(stored.workspace_id.clone()))
            .await?;
        let projection = summary(&stored);
        self.workspaces
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .insert(stored.workspace_id.clone(), stored);
        self.publish(assistant_protocol::RuntimeEvent::WorkspaceChanged {
            workspace_id: projection.workspace_id.clone(),
        });
        Ok(RegisterWorkspaceResult {
            workspace: projection,
            restored,
        })
    }

    /// 更新 Workspace 当前元数据；已创建 Session 的环境与 System Prompt 不参与此次变更。
    pub async fn update_workspace(
        &self,
        request: UpdateWorkspaceRequest,
    ) -> RuntimeResult<UpdateWorkspaceResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        let current = self.workspace(&request.workspace_id)?;
        if current.lifecycle == StoredWorkspaceLifecycle::Removed {
            return Err(RuntimeError::WorkspaceRemoved {
                workspace_id: request.workspace_id,
            });
        }
        let (label, primary_directory, additional_directories) = validate_workspace_form(
            request.label,
            request.primary_directory,
            request.additional_directories,
        )?;
        let stored = self
            .store
            .update_workspace(WorkspaceUpdate {
                workspace_id: request.workspace_id,
                label,
                requested_primary_directory: primary_directory,
                requested_additional_directories: additional_directories,
                changed_at_ms: super::now_ms()?,
            })
            .await
            .map_err(|source| {
                if source.kind() == StoreErrorKind::ResourceUnavailable {
                    RuntimeError::WorkspaceUnavailable {
                        workspace_id: current.workspace_id.clone(),
                    }
                } else {
                    RuntimeError::from_store("update workspace", source)
                }
            })?;
        let projection = summary(&stored);
        self.workspaces
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .insert(stored.workspace_id.clone(), stored);
        self.publish(assistant_protocol::RuntimeEvent::WorkspaceChanged {
            workspace_id: projection.workspace_id.clone(),
        });
        Ok(UpdateWorkspaceResult {
            workspace: projection,
        })
    }

    /// 查询 Workspace；已移除 Workspace 仍然保留可诊断投影。
    pub fn get_workspace(&self, request: GetWorkspaceRequest) -> RuntimeResult<GetWorkspaceResult> {
        let workspace = self.workspace(&request.workspace_id)?;
        Ok(GetWorkspaceResult {
            workspace: summary(&workspace),
        })
    }

    /// 按 Workspace ID 的确定性顺序列出活动 Workspace。
    pub fn list_workspaces(
        &self,
        _request: ListWorkspacesRequest,
    ) -> RuntimeResult<ListWorkspacesResult> {
        let workspaces = self
            .workspaces
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .values()
            .filter(|workspace| workspace.lifecycle == StoredWorkspaceLifecycle::Active)
            .map(summary)
            .collect();
        Ok(ListWorkspacesResult { workspaces })
    }

    /// 假删 Workspace；不触碰用户目录、Agent 私有目录或既有 Session。
    pub async fn remove_workspace(
        &self,
        request: RemoveWorkspaceRequest,
    ) -> RuntimeResult<RemoveWorkspaceResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        self.workspace(&request.workspace_id)?;
        let stored = self
            .store
            .remove_workspace(WorkspaceRemoval {
                workspace_id: request.workspace_id,
                changed_at_ms: super::now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("remove workspace", source))?;
        let projection = summary(&stored);
        self.workspaces
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .insert(stored.workspace_id.clone(), stored);
        self.publish(assistant_protocol::RuntimeEvent::WorkspaceChanged {
            workspace_id: projection.workspace_id.clone(),
        });
        Ok(RemoveWorkspaceResult {
            workspace: projection,
        })
    }

    pub(super) fn workspace_for_new_session(
        &self,
        workspace_id: &WorkspaceId,
    ) -> RuntimeResult<StoredWorkspace> {
        let workspace = self.workspace(workspace_id)?;
        if workspace.lifecycle == StoredWorkspaceLifecycle::Removed {
            return Err(RuntimeError::WorkspaceRemoved {
                workspace_id: workspace_id.clone(),
            });
        }
        Ok(workspace)
    }

    /// 解析既有 Session 已冻结的 Workspace 绑定。
    ///
    /// Session 持有的 Workspace ID 缺失表示运行环境已经不可用，而不是一次面向用户的
    /// Workspace 查询未命中；锁故障仍保留其内部状态错误，不能被降格成 unavailable。
    pub(super) fn workspace_for_session_context(
        &self,
        workspace_id: &WorkspaceId,
    ) -> RuntimeResult<StoredWorkspace> {
        match self.workspace(workspace_id) {
            Err(RuntimeError::WorkspaceNotFound { .. }) => {
                Err(RuntimeError::WorkspaceUnavailable {
                    workspace_id: workspace_id.clone(),
                })
            }
            result => result,
        }
    }

    fn workspace(&self, workspace_id: &WorkspaceId) -> RuntimeResult<StoredWorkspace> {
        self.workspaces
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace registry",
            })?
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| RuntimeError::WorkspaceNotFound {
                workspace_id: workspace_id.clone(),
            })
    }

    fn allocate_workspace_id(&self) -> RuntimeResult<WorkspaceId> {
        let workspaces =
            self.workspaces
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "workspace registry",
                })?;
        for _ in 0..id::GENERATION_ATTEMPTS {
            let value = id::generate("w").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "workspace id random source",
            })?;
            let workspace_id =
                WorkspaceId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "workspace id generator",
                })?;
            if !workspaces.contains_key(&workspace_id) {
                return Ok(workspace_id);
            }
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "workspace id collision",
        })
    }

    pub(super) fn session_workspace_snapshot(
        &self,
        session: &crate::session::SessionController,
    ) -> RuntimeResult<Option<assistant_protocol::SessionWorkspaceSnapshot>> {
        let environment = session.environment();
        let Some(workspace_id) = environment.workspace_id.as_ref() else {
            return Ok(None);
        };
        let current = self.workspace_for_session_context(workspace_id)?;
        Ok(Some(assistant_protocol::SessionWorkspaceSnapshot {
            workspace_id: workspace_id.clone(),
            label: current.label,
            primary_directory: environment.working_directory.clone(),
            additional_directories: environment.additional_workspace_directories.clone(),
            directories_match_current: current.user_directory == environment.working_directory
                && current.additional_directories == environment.additional_workspace_directories,
        }))
    }
}

fn validate_workspace_form(
    label: String,
    primary_directory: String,
    additional_directories: Vec<String>,
) -> RuntimeResult<(String, String, Vec<String>)> {
    let label = label.trim().to_owned();
    if label.is_empty()
        || label.chars().count() > MAX_WORKSPACE_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "workspace label must contain 1 to 80 non-control characters",
        });
    }
    if primary_directory.trim().is_empty() {
        return Err(RuntimeError::InvalidRequest {
            reason: "workspace primary directory must not be empty",
        });
    }
    if additional_directories.len() >= MAX_WORKSPACE_DIRECTORIES {
        return Err(RuntimeError::InvalidRequest {
            reason: "workspace must contain at most 16 directories",
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    if !seen.insert(primary_directory.as_str())
        || additional_directories
            .iter()
            .any(|directory| directory.trim().is_empty() || !seen.insert(directory.as_str()))
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "workspace directories must be non-empty and unique",
        });
    }
    Ok((label, primary_directory, additional_directories))
}
