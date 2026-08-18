//! 正式应用记忆状态及其 Store 业务命令。
//!
//! `agent-memory` 只描述可复用的模型记忆领域；时间、来源、修订与 CAS 属于
//! Assistant Runtime 的产品状态，因此在本模块包装而不下沉到领域 crate。

use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc};

use agent_memory::{
    MemoryPropertyValue, PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryFuture, PinnedMemoryId,
    PinnedMemoryLimits, PinnedMemoryPatch, PinnedMemoryStore, PinnedMemoryStoreError,
};
use assistant_protocol::SessionId;
use tokio_util::sync::CancellationToken;

use crate::{RuntimeStore, StoreError, StoreErrorKind};

pub const MAX_PERSONA_BYTES: usize = 32 * 1024;

pub fn pinned_memory_limits() -> PinnedMemoryLimits {
    PinnedMemoryLimits {
        max_entries: nonzero(256),
        max_id_bytes: nonzero(128),
        max_category_bytes: nonzero(128),
        max_content_bytes: nonzero(16 * 1024),
        max_attributes_per_entry: nonzero(32),
        max_attribute_key_bytes: nonzero(128),
        max_attribute_string_bytes: nonzero(4 * 1024),
        max_description_bytes: nonzero(4 * 1024),
        max_snapshot_bytes: nonzero(128 * 1024),
    }
}

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("memory limit is non-zero")
}

/// 创建 Pinned Memory 时记录的不可变来源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedMemoryCreatedBy {
    User,
    AgentTool { session_id: SessionId },
}

/// Runtime Store 中一条带产品元数据的 Pinned Memory。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPinnedMemory {
    pub entry: PinnedMemoryEntry,
    pub created_by: PinnedMemoryCreatedBy,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub revision: u64,
}

/// 单例 Persona 的当前权威投影。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersonaSnapshot {
    pub enabled: bool,
    pub content: String,
    pub revision: u64,
    pub updated_at_ms: i64,
}

/// Session 创建一次性读取的 Persona 与 Pinned 一致快照。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryContextSnapshot {
    pub persona: PersonaSnapshot,
    pub pinned_collection_revision: u64,
    pub pinned_memories: Vec<StoredPinnedMemory>,
}

/// 使用期望修订号替换单例 Persona。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersonaMutation {
    pub expected_revision: u64,
    pub enabled: bool,
    pub content: String,
    pub updated_at_ms: i64,
}

/// Pinned Memory 的原子 CAS 变更。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedMemoryMutation {
    Create {
        entry: PinnedMemoryEntry,
        created_by: PinnedMemoryCreatedBy,
        expected_collection_revision: u64,
        changed_at_ms: i64,
    },
    Replace {
        entry: PinnedMemoryEntry,
        expected_revision: u64,
        changed_at_ms: i64,
    },
    Delete {
        id: PinnedMemoryId,
        expected_revision: u64,
        changed_at_ms: i64,
    },
}

/// 一次 Pinned Memory 变更后的条目与集合修订。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedMemoryMutationResult {
    /// Create/Replace 返回最新条目；Delete 成功后为 `None`。
    pub memory: Option<StoredPinnedMemory>,
    pub collection_revision: u64,
}

/// 绑定调用方 Session 的 Pinned Memory 工具 Store。
///
/// 工具只能看见领域条目；来源、时间、revision 与 CAS 仍由 Runtime 统一管理。
pub struct RuntimePinnedMemoryStore {
    store: Arc<dyn RuntimeStore>,
    session_id: SessionId,
    limits: PinnedMemoryLimits,
}

impl RuntimePinnedMemoryStore {
    pub(crate) fn new(store: Arc<dyn RuntimeStore>, session_id: SessionId) -> Self {
        Self {
            store,
            session_id,
            limits: pinned_memory_limits(),
        }
    }

    async fn context(&self) -> Result<MemoryContextSnapshot, PinnedMemoryStoreError> {
        self.store
            .load_memory_context()
            .await
            .map_err(map_store_error)
    }
}

impl PinnedMemoryStore for RuntimePinnedMemoryStore {
    fn list(
        &self,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            Ok(self
                .context()
                .await?
                .pinned_memories
                .into_iter()
                .map(|stored| stored.entry)
                .collect())
        })
    }

    fn pin(
        &self,
        draft: PinnedMemoryDraft,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            draft.validate(&self.limits).map_err(validation_error)?;
            let context = self.context().await?;
            if context.pinned_memories.len() >= self.limits.max_entries.get() {
                return Err(PinnedMemoryStoreError::CapacityExceeded {
                    message: "entry limit reached".to_owned(),
                });
            }
            let entry = PinnedMemoryEntry {
                id: allocate_memory_id()?,
                category: draft.category,
                content: draft.content,
                attributes: draft.attributes,
            };
            let result = self
                .store
                .mutate_pinned_memory(PinnedMemoryMutation::Create {
                    entry: entry.clone(),
                    created_by: PinnedMemoryCreatedBy::AgentTool {
                        session_id: self.session_id.clone(),
                    },
                    expected_collection_revision: context.pinned_collection_revision,
                    changed_at_ms: crate::runtime::now_ms().map_err(runtime_error)?,
                })
                .await
                .map_err(map_store_error)?;
            Ok(result.memory.map_or(entry, |memory| memory.entry))
        })
    }

    fn update(
        &self,
        id: PinnedMemoryId,
        patch: PinnedMemoryPatch,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            patch.validate(&self.limits).map_err(validation_error)?;
            let context = self.context().await?;
            let stored = context
                .pinned_memories
                .into_iter()
                .find(|memory| memory.entry.id == id)
                .ok_or_else(|| PinnedMemoryStoreError::NotFound { id: id.clone() })?;
            let entry = apply_patch(stored.entry, patch);
            entry.validate(&self.limits).map_err(validation_error)?;
            let result = self
                .store
                .mutate_pinned_memory(PinnedMemoryMutation::Replace {
                    entry: entry.clone(),
                    expected_revision: stored.revision,
                    changed_at_ms: crate::runtime::now_ms().map_err(runtime_error)?,
                })
                .await
                .map_err(map_store_error)?;
            Ok(result.memory.map_or(entry, |memory| memory.entry))
        })
    }

    fn unpin(
        &self,
        id: PinnedMemoryId,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            let context = self.context().await?;
            let stored = context
                .pinned_memories
                .into_iter()
                .find(|memory| memory.entry.id == id)
                .ok_or_else(|| PinnedMemoryStoreError::NotFound { id: id.clone() })?;
            self.store
                .mutate_pinned_memory(PinnedMemoryMutation::Delete {
                    id,
                    expected_revision: stored.revision,
                    changed_at_ms: crate::runtime::now_ms().map_err(runtime_error)?,
                })
                .await
                .map_err(map_store_error)?;
            Ok(stored.entry)
        })
    }
}

fn allocate_memory_id() -> Result<PinnedMemoryId, PinnedMemoryStoreError> {
    let value = crate::id::generate("pm").map_err(|_| PinnedMemoryStoreError::Io {
        message: "secure memory id generation failed".to_owned(),
    })?;
    PinnedMemoryId::new(value).map_err(validation_error)
}

fn apply_patch(mut entry: PinnedMemoryEntry, patch: PinnedMemoryPatch) -> PinnedMemoryEntry {
    if let Some(category) = patch.category {
        entry.category = category;
    }
    if let Some(content) = patch.content {
        entry.content = content;
    }
    if let Some(attributes) = patch.attributes {
        entry.attributes = attributes;
    }
    entry
}

fn ensure_not_cancelled(token: &CancellationToken) -> Result<(), PinnedMemoryStoreError> {
    if token.is_cancelled() {
        Err(PinnedMemoryStoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn validation_error(error: impl std::fmt::Display) -> PinnedMemoryStoreError {
    PinnedMemoryStoreError::InvalidInput {
        message: error.to_string(),
    }
}

fn runtime_error(error: impl std::fmt::Display) -> PinnedMemoryStoreError {
    PinnedMemoryStoreError::Io {
        message: error.to_string(),
    }
}

fn map_store_error(error: StoreError) -> PinnedMemoryStoreError {
    match error.kind() {
        StoreErrorKind::Conflict => PinnedMemoryStoreError::Io {
            message: "pinned memory changed concurrently; retry the operation".to_owned(),
        },
        StoreErrorKind::InvalidData => PinnedMemoryStoreError::Corrupt {
            message: error.message().to_owned(),
        },
        StoreErrorKind::InvalidInput => PinnedMemoryStoreError::InvalidInput {
            message: error.message().to_owned(),
        },
        _ => PinnedMemoryStoreError::Io {
            message: error.message().to_owned(),
        },
    }
}

pub(crate) fn protocol_attributes(
    attributes: &BTreeMap<String, MemoryPropertyValue>,
) -> BTreeMap<String, assistant_protocol::MemoryAttributeValue> {
    attributes
        .iter()
        .map(|(key, value)| {
            let value = match value {
                MemoryPropertyValue::String(value) => {
                    assistant_protocol::MemoryAttributeValue::String(value.clone())
                }
                MemoryPropertyValue::Number(value) => {
                    assistant_protocol::MemoryAttributeValue::Number(value.to_string())
                }
            };
            (key.clone(), value)
        })
        .collect()
}

pub(crate) fn domain_attributes(
    attributes: BTreeMap<String, assistant_protocol::MemoryAttributeValue>,
) -> Result<BTreeMap<String, MemoryPropertyValue>, PinnedMemoryStoreError> {
    attributes
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                assistant_protocol::MemoryAttributeValue::String(value) => {
                    MemoryPropertyValue::String(value)
                }
                assistant_protocol::MemoryAttributeValue::Number(value) => {
                    let number = value
                        .parse::<serde_json::Number>()
                        .map_err(validation_error)?;
                    MemoryPropertyValue::Number(number)
                }
            };
            Ok((key, value))
        })
        .collect()
}
