//! 子任务 OS 临时空间的 Host 实现。

use assistant_protocol::ChildTaskId;
use assistant_runtime::{
    ChildTaskWorkspaceError, ChildTaskWorkspaceFactory, ChildTaskWorkspaceFuture,
    ChildTaskWorkspaceLease,
};

pub(super) struct HostChildTaskWorkspaceFactory;

struct HostChildTaskWorkspaceLease {
    path: String,
    // TempDir 的 Drop 是临时目录生命周期的唯一清理责任人。
    _directory: tempfile::TempDir,
}

impl ChildTaskWorkspaceLease for HostChildTaskWorkspaceLease {
    fn path(&self) -> &str {
        &self.path
    }
}

impl ChildTaskWorkspaceFactory for HostChildTaskWorkspaceFactory {
    fn create<'a>(&'a self, child_task_id: &'a ChildTaskId) -> ChildTaskWorkspaceFuture<'a> {
        let prefix = format!("ez-assistant-{}-", child_task_id.as_str());
        Box::pin(async move {
            let directory = tokio::task::spawn_blocking(move || {
                tempfile::Builder::new().prefix(&prefix).tempdir()
            })
            .await
            .map_err(ChildTaskWorkspaceError::with_source)?
            .map_err(ChildTaskWorkspaceError::with_source)?;
            let path = directory
                .path()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    ChildTaskWorkspaceError::with_source(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "temporary path is not UTF-8",
                    ))
                })?;
            Ok(Box::new(HostChildTaskWorkspaceLease {
                path,
                _directory: directory,
            }) as Box<dyn ChildTaskWorkspaceLease>)
        })
    }
}
