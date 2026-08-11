//! Workspace 的 Runtime 内部投影与协议转换。

use assistant_protocol::{WorkspaceLifecycle, WorkspaceSummary};

use crate::{StoredWorkspace, StoredWorkspaceLifecycle};

pub(crate) fn summary(stored: &StoredWorkspace) -> WorkspaceSummary {
    WorkspaceSummary {
        workspace_id: stored.workspace_id.clone(),
        user_directory: stored.user_directory.clone(),
        agent_directory: stored.agent_directory.clone(),
        lifecycle: match stored.lifecycle {
            StoredWorkspaceLifecycle::Active => WorkspaceLifecycle::Active,
            StoredWorkspaceLifecycle::Removed => WorkspaceLifecycle::Removed,
        },
        created_at_ms: stored.created_at_ms,
        updated_at_ms: stored.updated_at_ms,
        removed_at_ms: stored.removed_at_ms,
    }
}
