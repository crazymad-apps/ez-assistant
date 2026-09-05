//! Session 文件根的只读权威解析入口。

use assistant_protocol::{SessionId, SessionResourceRoot};

use crate::{AssistantRuntime, RuntimeError, RuntimeResult, SessionExecutionEnvironment};

impl AssistantRuntime {
    /// 将协议中的根身份解析为 Session 创建时冻结的物理根。
    ///
    /// 返回值仅供同进程 Host 在执行路径校验和 I/O 时使用，不进入 Session View 或 Desktop WebView。
    pub fn resolve_session_resource_root(
        &self,
        session_id: &SessionId,
        root: &SessionResourceRoot,
    ) -> RuntimeResult<String> {
        let session = self.session(session_id)?;
        resolve_root_from_environment(session.environment(), root)
    }
}

fn resolve_root_from_environment(
    environment: &SessionExecutionEnvironment,
    root: &SessionResourceRoot,
) -> RuntimeResult<String> {
    match root {
        SessionResourceRoot::WorkspacePrimary if environment.workspace_id.is_some() => {
            Ok(environment.working_directory.clone())
        }
        SessionResourceRoot::WorkspaceAdditional { directory_index }
            if environment.workspace_id.is_some() =>
        {
            environment
                .additional_workspace_directories
                .get(*directory_index as usize)
                .cloned()
                .ok_or(RuntimeError::InvalidRequest {
                    reason: "workspace directory index is invalid",
                })
        }
        SessionResourceRoot::SessionPrivate => Ok(environment.session_private_directory.clone()),
        SessionResourceRoot::WorkspacePrimary | SessionResourceRoot::WorkspaceAdditional { .. } => {
            Err(RuntimeError::InvalidRequest {
                reason: "session has no workspace resource root",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{SessionResourceRoot, WorkspaceId};

    use super::*;

    fn environment(workspace: bool) -> SessionExecutionEnvironment {
        SessionExecutionEnvironment {
            workspace_id: workspace.then(|| WorkspaceId::new("workspace-1").expect("workspace")),
            working_directory: if workspace {
                "/workspace"
            } else {
                "/session/private"
            }
            .to_owned(),
            additional_workspace_directories: if workspace {
                vec!["/workspace-docs".to_owned()]
            } else {
                Vec::new()
            },
            workspace_private_directory: None,
            session_attachment_directory: "/session/attachments".to_owned(),
            session_tool_image_directory: "/session/tool-images".to_owned(),
            session_private_directory: "/session/private".to_owned(),
        }
    }

    #[test]
    fn resolves_only_frozen_workspace_roots() {
        let environment = environment(true);
        assert_eq!(
            resolve_root_from_environment(&environment, &SessionResourceRoot::WorkspacePrimary)
                .expect("primary root"),
            "/workspace"
        );
        assert_eq!(
            resolve_root_from_environment(
                &environment,
                &SessionResourceRoot::WorkspaceAdditional { directory_index: 0 },
            )
            .expect("additional root"),
            "/workspace-docs"
        );
        assert!(
            resolve_root_from_environment(
                &environment,
                &SessionResourceRoot::WorkspaceAdditional { directory_index: 1 },
            )
            .is_err()
        );
    }

    #[test]
    fn unbound_session_does_not_alias_private_as_workspace_primary() {
        let environment = environment(false);
        assert!(
            resolve_root_from_environment(&environment, &SessionResourceRoot::WorkspacePrimary)
                .is_err()
        );
        assert_eq!(
            resolve_root_from_environment(&environment, &SessionResourceRoot::SessionPrivate)
                .expect("private root"),
            "/session/private"
        );
    }
}
