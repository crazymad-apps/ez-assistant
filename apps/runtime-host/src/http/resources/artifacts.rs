//! 交付声明与客户端可用性检查共用既有 Session 资源读取边界。

use super::{files, host, resolve_session_resource_path};
use crate::user_paths::UserPaths;
use agent_tools::{ToolError, ToolExecuteFuture};
use assistant_protocol::{SessionResourceFileInfo, SessionResourceLocator, SessionResourceRoot};
use assistant_runtime::{ArtifactFiles, SessionExecutionEnvironment};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub(crate) struct HostArtifactFiles {
    environment: SessionExecutionEnvironment,
    paths: Arc<UserPaths>,
}

impl HostArtifactFiles {
    pub(crate) fn new(environment: SessionExecutionEnvironment, paths: Arc<UserPaths>) -> Self {
        Self { environment, paths }
    }
    fn roots(&self) -> Vec<(SessionResourceRoot, &str)> {
        let mut roots = vec![(
            SessionResourceRoot::SessionPrivate,
            self.environment.session_private_directory.as_str(),
        )];
        if self.environment.workspace_id.is_some() {
            roots.push((
                SessionResourceRoot::WorkspacePrimary,
                &self.environment.working_directory,
            ));
            roots.extend(
                self.environment
                    .additional_workspace_directories
                    .iter()
                    .enumerate()
                    .map(|(index, path)| {
                        (
                            SessionResourceRoot::WorkspaceAdditional {
                                directory_index: index as u32,
                            },
                            path.as_str(),
                        )
                    }),
            );
        }
        // 重叠目录优先最具体根，避免同一私有文件得到不同身份。
        roots.sort_by_key(|(_, path)| std::cmp::Reverse(Path::new(path).components().count()));
        roots
    }
}

impl ArtifactFiles for HostArtifactFiles {
    fn locate(&self, path: &str) -> Result<SessionResourceLocator, ToolError> {
        if path.is_empty() || path.contains('\0') || path.len() > 4096 {
            return Err(ToolError::invalid_input("artifact path is invalid"));
        }
        let path = Path::new(path);
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(ToolError::invalid_input(
                "artifact path cannot contain parent traversal",
            ));
        }
        let absolute: PathBuf = if path.is_absolute() {
            path.to_owned()
        } else {
            Path::new(&self.environment.working_directory).join(path)
        }
        .components()
        .collect();
        for (root, directory) in self.roots() {
            if let Ok(relative) = absolute.strip_prefix(directory) {
                let parts = relative
                    .components()
                    .map(|part| match part {
                        Component::Normal(value) => value
                            .to_str()
                            .ok_or_else(|| ToolError::invalid_input("artifact path must be UTF-8")),
                        _ => Err(ToolError::invalid_input("artifact path is invalid")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if parts.is_empty() {
                    return Err(ToolError::invalid_input("artifact must be a file"));
                }
                return Ok(SessionResourceLocator {
                    root,
                    relative_path: parts.join("/"),
                });
            }
        }
        Err(ToolError::invalid_input(
            "artifact is outside this session's resource roots",
        ))
    }
    fn check<'a>(&'a self, locator: &'a SessionResourceLocator) -> ToolExecuteFuture<'a, ()> {
        Box::pin(async move {
            let root = self
                .roots()
                .into_iter()
                .find(|(root, _)| root == &locator.root)
                .map(|(_, path)| path)
                .ok_or_else(|| ToolError::invalid_input("artifact root is unavailable"))?;
            check_file(root, locator, self.paths.clone())
                .await
                .map(|_| ())
                .map_err(|error| ToolError::execution(error.message))
        })
    }
}

async fn check_file(
    root: &str,
    locator: &SessionResourceLocator,
    paths: Arc<UserPaths>,
) -> Result<SessionResourceFileInfo, assistant_protocol::RuntimeErrorInfo> {
    let (_, path) = resolve_session_resource_path(root, locator).await?;
    host::read(move || {
        let path = paths.resolve(&path, false).map_err(files::error)?;
        let file = files::open_resolved(&path, false)?;
        Ok(SessionResourceFileInfo {
            size_bytes: file.metadata().map_err(files::error)?.len(),
        })
    })
    .await
}

pub(in crate::http) async fn stat_session_resource_file(
    axum::extract::State(state): axum::extract::State<super::super::HttpState>,
    axum::Extension(services): axum::Extension<Arc<super::super::ReadyServices>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
    axum::Json(request): axum::Json<assistant_protocol::PreviewSessionResourceFileRequest>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let session_id = match assistant_protocol::SessionId::new(session_id) {
        Ok(id) => id,
        Err(_) => return super::resource_error(super::invalid_request("session id is invalid")),
    };
    let root = match services
        .runtime
        .resolve_session_resource_root(&session_id, &request.locator.root)
        .await
    {
        Ok(root) => root,
        Err(error) => return super::resource_error(error.to_protocol_info()),
    };
    let _permit = match state.file_reads.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return super::resource_error(super::invalid_request("文件读取繁忙，请稍后重试。"));
        }
    };
    match check_file(&root, &request.locator, services.paths.clone()).await {
        Ok(info) => axum::Json(info).into_response(),
        Err(error) => super::resource_error(error),
    }
}
