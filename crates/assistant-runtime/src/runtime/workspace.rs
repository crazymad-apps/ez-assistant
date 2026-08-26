//! Workspace Registry 的业务命令与 Session 绑定查询。

use assistant_protocol::{
    GetWorkspaceRequest, GetWorkspaceResult, ListWorkspacesRequest, ListWorkspacesResult,
    RegisterWorkspaceRequest, RegisterWorkspaceResult, RemoveWorkspaceRequest,
    RemoveWorkspaceResult, WorkspaceId,
};

use super::AssistantRuntime;
use crate::{
    NewWorkspaceRegistration, RuntimeError, RuntimeResult, StoreErrorKind, StoredWorkspace,
    StoredWorkspaceLifecycle, WorkspaceRemoval, id, permission::PermissionFileScope,
    workspace::summary,
};

impl AssistantRuntime {
    /// 按 canonical path 幂等登记 Workspace；重新登记已移除路径会恢复原 ID。
    pub async fn register_workspace(
        &self,
        request: RegisterWorkspaceRequest,
    ) -> RuntimeResult<RegisterWorkspaceResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.workspace_mutation_gate.lock().await;
        if request.path.trim().is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "workspace path must not be empty",
            });
        }
        let workspace_id = self.allocate_workspace_id()?;
        let stored = self
            .store
            .register_workspace(NewWorkspaceRegistration {
                workspace_id: workspace_id.clone(),
                requested_directory: request.path,
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
}
