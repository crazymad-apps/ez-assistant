//! Attachment 查询、幂等准入与上传完成提交。

use agent_types::FileReference;
use assistant_protocol::{
    AttachmentId, AttachmentState, AttachmentSummary, GetAttachmentRequest, GetAttachmentResult,
    ListAttachmentsRequest, ListAttachmentsResult, SessionId, UploadAttachmentResult,
};

use super::AssistantRuntime;
use crate::{
    NewAttachmentUpload, RuntimeError, RuntimeResult, StoredAttachment, StoredAttachmentState, id,
};

/// Host 已完成流式接收与摘要计算、交回 Runtime 提交的事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedAttachmentUpload {
    pub session_id: SessionId,
    pub original_name: String,
    pub staging_path: String,
    /// 原始文件名与文件字节共同决定的 Blob 身份摘要。
    pub blob_hash: String,
    pub size_bytes: u64,
}

impl AssistantRuntime {
    /// 在 Host 读取上传正文前检查 Session 生命周期；内容去重必须等 Hash 形成后完成。
    pub async fn begin_attachment_upload(&self, session_id: &SessionId) -> RuntimeResult<()> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        Ok(())
    }

    /// 将 Host staging 文件原子落实为 Blob、Session 稳定视图和业务元数据。
    pub async fn finalize_attachment_upload(
        &self,
        upload: StagedAttachmentUpload,
    ) -> RuntimeResult<UploadAttachmentResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&upload.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        // 同 Session 的 Blob Hash 去重由 Store 最终核对并清理多余 staging；这覆盖两个
        // HTTP 请求同时完成同名且同内容文件上传的竞态。
        let attachment_id = self.allocate_attachment_id()?;
        let stored = self
            .store
            .upload_attachment(NewAttachmentUpload {
                attachment_id,
                session_id: upload.session_id,
                original_name: upload.original_name,
                staging_path: upload.staging_path,
                blob_hash: upload.blob_hash,
                size_bytes: upload.size_bytes,
                created_at_ms: super::now_ms()?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("upload attachment", source))?;
        let projection = summary(&stored);
        self.attachments
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment registry",
            })?
            .insert(stored.attachment_id.clone(), stored);
        self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
            session_id: projection.session_id.clone(),
        });
        Ok(UploadAttachmentResult {
            attachment: projection,
        })
    }

    pub fn get_attachment(
        &self,
        request: GetAttachmentRequest,
    ) -> RuntimeResult<GetAttachmentResult> {
        self.session(&request.session_id)?;
        let attachment = self
            .attachments
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment registry",
            })?
            .get(&request.attachment_id)
            .filter(|attachment| attachment.session_id == request.session_id)
            .cloned()
            .ok_or(RuntimeError::AttachmentNotFound {
                session_id: request.session_id,
                attachment_id: request.attachment_id,
            })?;
        Ok(GetAttachmentResult {
            attachment: summary(&attachment),
        })
    }

    pub fn list_attachments(
        &self,
        request: ListAttachmentsRequest,
    ) -> RuntimeResult<ListAttachmentsResult> {
        self.session(&request.session_id)?;
        let mut attachments = self
            .attachments
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment registry",
            })?
            .values()
            .filter(|attachment| attachment.session_id == request.session_id)
            .map(summary)
            .collect::<Vec<_>>();
        attachments.sort_by(|left, right| {
            (left.created_at_ms, left.attachment_id.as_str())
                .cmp(&(right.created_at_ms, right.attachment_id.as_str()))
        });
        Ok(ListAttachmentsResult { attachments })
    }

    /// 把有序应用层 ID 冻结为规范 User Part 所需的文件信息。
    ///
    /// 其他 Session 的 ID 与不存在的 ID 统一按 not found 处理，避免跨 Session 探测。
    pub(super) fn resolve_file_references(
        &self,
        session_id: &SessionId,
        attachment_ids: &[AttachmentId],
    ) -> RuntimeResult<Vec<FileReference>> {
        let attachments =
            self.attachments
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "attachment registry",
                })?;
        let mut seen = std::collections::BTreeSet::new();
        let mut files = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            if !seen.insert(attachment_id) {
                return Err(RuntimeError::InvalidRequest {
                    reason: "attachment ids must not contain duplicates",
                });
            }
            let attachment = attachments
                .get(attachment_id)
                .filter(|attachment| &attachment.session_id == session_id)
                .ok_or_else(|| RuntimeError::AttachmentNotFound {
                    session_id: session_id.clone(),
                    attachment_id: attachment_id.clone(),
                })?;
            if attachment.state != StoredAttachmentState::Ready {
                return Err(RuntimeError::AttachmentUnavailable {
                    session_id: session_id.clone(),
                    attachment_id: attachment_id.clone(),
                });
            }
            files.push(FileReference {
                original_name: attachment.original_name.clone(),
                readable_path: attachment.agent_readable_path.clone(),
            });
        }
        Ok(files)
    }

    pub(super) fn allocate_attachment_id(&self) -> RuntimeResult<AttachmentId> {
        let attachments =
            self.attachments
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "attachment registry",
                })?;
        for _ in 0..id::GENERATION_ATTEMPTS {
            let value = id::generate("a").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "attachment id random source",
            })?;
            let attachment_id =
                AttachmentId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "attachment id generator",
                })?;
            if !attachments.contains_key(&attachment_id) {
                return Ok(attachment_id);
            }
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "attachment id collision",
        })
    }
}

fn summary(stored: &StoredAttachment) -> AttachmentSummary {
    AttachmentSummary {
        attachment_id: stored.attachment_id.clone(),
        session_id: stored.session_id.clone(),
        original_name: stored.original_name.clone(),
        size_bytes: stored.size_bytes,
        agent_readable_path: stored.agent_readable_path.clone(),
        state: match stored.state {
            StoredAttachmentState::Ready => AttachmentState::Ready,
            StoredAttachmentState::Unavailable => AttachmentState::Unavailable,
        },
        created_at_ms: stored.created_at_ms,
    }
}
