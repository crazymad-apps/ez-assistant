use assistant_protocol::{AttachmentId, SessionId, WorkspaceId};

/// Workspace 的持久化生命周期。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredWorkspaceLifecycle {
    Active,
    Removed,
}

/// Host Store 恢复或写入完成的 Workspace 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredWorkspace {
    pub workspace_id: WorkspaceId,
    pub user_directory: String,
    pub agent_directory: String,
    pub lifecycle: StoredWorkspaceLifecycle,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub removed_at_ms: Option<i64>,
}

/// Attachment 的正文及 Session 稳定视图是否可供 Agent 读取。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredAttachmentState {
    Ready,
    Unavailable,
}

/// Host Store 恢复或写入完成的 Attachment 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAttachment {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    /// 由原始文件名和文件字节共同计算的 Blob 身份摘要。
    pub blob_hash: String,
    pub size_bytes: u64,
    /// Host 按文件签名检测的实际 MIME；旧 Blob 可以为空。
    pub media_type: Option<String>,
    pub agent_readable_path: String,
    pub state: StoredAttachmentState,
    pub created_at_ms: i64,
}

/// Host 已流式接收并校验、等待 Store 原子完成的 Attachment 上传。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAttachmentUpload {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    pub staging_path: String,
    /// 由原始文件名和文件字节共同计算的 Blob 身份摘要。
    pub blob_hash: String,
    pub size_bytes: u64,
    pub media_type: Option<String>,
    pub created_at_ms: i64,
}

/// Runtime 请求 Store 登记或按 canonical path 恢复 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewWorkspaceRegistration {
    pub workspace_id: WorkspaceId,
    pub requested_directory: String,
    pub changed_at_ms: i64,
}

/// Runtime 请求 Store 假删 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRemoval {
    pub workspace_id: WorkspaceId,
    pub changed_at_ms: i64,
}
