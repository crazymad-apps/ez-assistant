use std::path::{Path, PathBuf};

use assistant_protocol::{AttachmentId, SessionId, WorkspaceId};

use super::{AcceptedInput, NewStoredInput, NewStoredSession, StoredGoal, StoredSession};

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
    pub label: String,
    pub user_directory: String,
    pub additional_directories: Vec<String>,
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

/// 新会话首次发送需要由同一 Store 业务操作提交的完整事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredSessionMaterialization {
    pub session: NewStoredSession,
    pub attachments: Vec<NewAttachmentUpload>,
    pub input: NewStoredInput,
}

/// Store 已可靠提交或按 materialization key 恢复的首次发送结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSessionMaterialization {
    pub session: StoredSession,
    pub attachments: Vec<StoredAttachment>,
    pub accepted: AcceptedInput,
    pub goal: Option<StoredGoal>,
}

/// 生成 Session Attachment 稳定视图路径。
///
/// Runtime 用它在可靠提交前冻结规范 `FileReference`，Host Store 使用同一约定创建实际视图。
pub fn attachment_stable_view_path(
    attachment_directory: &Path,
    attachment_id: &AttachmentId,
    original_name: &str,
) -> PathBuf {
    attachment_directory
        .join(attachment_id.as_str())
        .join(safe_attachment_display_name(original_name))
}

fn safe_attachment_display_name(name: &str) -> String {
    let mut result = String::new();
    for character in name.chars() {
        let character = if character.is_control() {
            '_'
        } else {
            character
        };
        if result.len() + character.len_utf8() > 180 {
            break;
        }
        result.push(character);
    }
    if result.is_empty() || result == "." || result == ".." {
        "attachment".to_owned()
    } else {
        result
    }
}

/// Runtime 请求 Store 登记或按 canonical path 恢复 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewWorkspaceRegistration {
    pub workspace_id: WorkspaceId,
    pub label: String,
    pub requested_primary_directory: String,
    pub requested_additional_directories: Vec<String>,
    pub changed_at_ms: i64,
}

/// Runtime 请求 Store 更新一条 Workspace 的完整当前元数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceUpdate {
    pub workspace_id: WorkspaceId,
    pub label: String,
    pub requested_primary_directory: String,
    pub requested_additional_directories: Vec<String>,
    pub changed_at_ms: i64,
}

/// Runtime 请求 Store 假删 Workspace。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRemoval {
    pub workspace_id: WorkspaceId,
    pub changed_at_ms: i64,
}
