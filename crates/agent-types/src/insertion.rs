//! Provider-neutral 的内部上下文与请求期插入计划。

use serde::{Deserialize, Deserializer};
use thiserror::Error;

use crate::{MessageId, PartId, ToolCallId, ToolImageReference};

/// 一段需要进入规范 UserMessage、但不属于用户正文的结构化上下文。
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct InternalContextPart {
    /// 片段 ID，用于规范消息持久化与回放。
    pub id: PartId,
    /// 跨消息和控制事实关联的稳定边界身份。
    pub boundary_id: String,
    /// 不解释正文的稳定来源类别。
    pub kind: String,
    /// 压缩时需要保留最新版本的逻辑上下文键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_key: Option<String>,
    /// 已冻结、可直接投影给模型的正文。
    pub text: String,
}

impl<'de> Deserialize<'de> for InternalContextPart {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInternalContextPart {
            id: PartId,
            boundary_id: String,
            kind: String,
            #[serde(default)]
            retention_key: Option<String>,
            text: String,
        }

        let raw = RawInternalContextPart::deserialize(deserializer)?;
        Self::new(
            raw.id,
            raw.boundary_id,
            raw.kind,
            raw.retention_key,
            raw.text,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl InternalContextPart {
    /// 创建并校验一段内部上下文。
    pub fn new(
        id: PartId,
        boundary_id: impl Into<String>,
        kind: impl Into<String>,
        retention_key: Option<String>,
        text: impl Into<String>,
    ) -> Result<Self, InternalContextError> {
        let boundary_id = boundary_id.into();
        if boundary_id.trim().is_empty() {
            return Err(InternalContextError::EmptyBoundaryId);
        }
        let kind = kind.into();
        if kind.trim().is_empty() {
            return Err(InternalContextError::EmptyKind);
        }
        if retention_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty())
        {
            return Err(InternalContextError::EmptyRetentionKey);
        }
        let text = text.into();
        if text.is_empty() {
            return Err(InternalContextError::EmptyText);
        }
        Ok(Self {
            id,
            boundary_id,
            kind,
            retention_key,
            text,
        })
    }
}

/// 内部上下文不满足规范边界时返回的错误。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum InternalContextError {
    #[error("internal context boundary id must not be empty")]
    EmptyBoundaryId,
    #[error("internal context kind must not be empty")]
    EmptyKind,
    #[error("internal context retention key must not be empty")]
    EmptyRetentionKey,
    #[error("internal context text must not be empty")]
    EmptyText,
}

/// 插入内容相对于规范消息的位置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextInsertionPlacement {
    /// 作为一个 Part 进入既有或新建的规范 UserMessage。
    UserMessagePart,
    /// 在完整 Tool Result 批次之后、下一条规范消息之前投影。
    AfterToolResultBatch,
    /// 只在当次请求的规范历史末尾追加。
    EndOfRequest,
}

/// 插入内容是否属于规范持久事实。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextInsertionPersistence {
    /// 必须先进入规范 Conversation，再供模型请求消费。
    Canonical,
    /// 仅由已冻结规范事实确定性派生，不写回 Conversation。
    RequestOnly,
}

/// 插入内容的产品可见性。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextInsertionVisibility {
    /// 模型可见，产品转录隐藏。
    ModelOnly,
    /// 内容属于产品可见消息的一部分。
    ProductVisible,
}

/// request-only 图片信封中的一张有序图片。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResultImageInsertion {
    /// 图片所属的规范 ToolMessage。
    pub tool_message_id: MessageId,
    /// 图片所属的 Tool Call。
    pub call_id: ToolCallId,
    /// 图片在 Tool Result Parts 中的原始下标。
    pub part_index: usize,
    /// 已落账的 Session Tool Image 引用。
    pub image: ToolImageReference,
    /// Tool Result 字符串和图片信封共用的确定性标签。
    pub label: String,
}

/// 插入计划携带的 Provider-neutral 载荷。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextInsertionPayload {
    /// 规范或请求期内部文本上下文。
    InternalContext(InternalContextPart),
    /// 从完整 Tool Result 批次派生的有序图片信封。
    ToolResultImages(Vec<ToolResultImageInsertion>),
}

/// 一份已冻结来源、位置、存储和可见性语义的插入计划。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextInsertionPlan<T> {
    pub source_identity: T,
    pub placement: ContextInsertionPlacement,
    pub persistence: ContextInsertionPersistence,
    pub visibility: ContextInsertionVisibility,
    pub payload: ContextInsertionPayload,
}

impl<T> ContextInsertionPlan<T> {
    /// 创建规范 UserMessage 内部 Part 计划。
    pub fn canonical_internal(source_identity: T, part: InternalContextPart) -> Self {
        Self {
            source_identity,
            placement: ContextInsertionPlacement::UserMessagePart,
            persistence: ContextInsertionPersistence::Canonical,
            visibility: ContextInsertionVisibility::ModelOnly,
            payload: ContextInsertionPayload::InternalContext(part),
        }
    }

    /// 创建只存在于单次模型请求末尾的内部 Part 计划。
    pub fn request_only_internal(source_identity: T, part: InternalContextPart) -> Self {
        Self {
            source_identity,
            placement: ContextInsertionPlacement::EndOfRequest,
            persistence: ContextInsertionPersistence::RequestOnly,
            visibility: ContextInsertionVisibility::ModelOnly,
            payload: ContextInsertionPayload::InternalContext(part),
        }
    }

    /// 创建完整 Tool Result 批次后的 request-only 图片信封计划。
    pub fn request_only_tool_images(
        source_identity: T,
        images: Vec<ToolResultImageInsertion>,
    ) -> Self {
        Self {
            source_identity,
            placement: ContextInsertionPlacement::AfterToolResultBatch,
            persistence: ContextInsertionPersistence::RequestOnly,
            visibility: ContextInsertionVisibility::ModelOnly,
            payload: ContextInsertionPayload::ToolResultImages(images),
        }
    }
}
