use std::{future::Future, pin::Pin};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryId, PinnedMemoryPatch};

/// Pinned Memory Store 操作返回的 boxed future。
pub type PinnedMemoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PinnedMemoryStoreError>> + Send + 'a>>;

/// Pinned Memory Store 的稳定错误分类。
///
/// 错误只携带受控诊断，不包含完整记忆正文或底层路径。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PinnedMemoryStoreError {
    /// 输入不满足领域或 Store 不变量。
    #[error("invalid pinned memory input: {message}")]
    InvalidInput {
        /// 脱敏诊断。
        message: String,
    },
    /// 指定 ID 不存在。
    #[error("pinned memory `{id}` was not found")]
    NotFound {
        /// 未找到的稳定 ID。
        id: PinnedMemoryId,
    },
    /// 写入会超过显式容量上限。
    #[error("pinned memory capacity exceeded: {message}")]
    CapacityExceeded {
        /// 不包含正文的容量诊断。
        message: String,
    },
    /// 权威数据损坏或版本不兼容。
    #[error("pinned memory store is corrupt: {message}")]
    Corrupt {
        /// 不包含原始数据的诊断。
        message: String,
    },
    /// Store I/O 失败。
    #[error("pinned memory store I/O failed: {message}")]
    Io {
        /// 不包含路径和正文的诊断。
        message: String,
    },
    /// 调用已取消。
    #[error("pinned memory operation was cancelled")]
    Cancelled,
}

/// Pinned Memory 工具背后的最小持久能力。
///
/// trait 不规定本地、远程或数据库实现，也不暴露 revision、通用 CRUD、自动合并或淘汰。
pub trait PinnedMemoryStore: Send + Sync {
    /// 列出 Store 的最新完整状态。
    fn list(
        &self,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>>;

    /// 原子分配稳定 ID 并保存新条目。
    fn pin(
        &self,
        draft: PinnedMemoryDraft,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry>;

    /// 修改存在的条目并返回保存后的完整状态。
    fn update(
        &self,
        id: PinnedMemoryId,
        patch: PinnedMemoryPatch,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry>;

    /// 删除存在的条目并返回被删除的完整状态。
    fn unpin(
        &self,
        id: PinnedMemoryId,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry>;
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use crate::{MemoryPropertyValue, PinnedMemoryCategory};

    use super::*;

    struct MiniStore {
        entries: Mutex<Vec<PinnedMemoryEntry>>,
    }

    impl PinnedMemoryStore for MiniStore {
        fn list(
            &self,
            cancellation: CancellationToken,
        ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                Ok(self.entries.lock().expect("lock entries").clone())
            })
        }

        fn pin(
            &self,
            draft: PinnedMemoryDraft,
            cancellation: CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                let mut entries = self.entries.lock().expect("lock entries");
                let entry = PinnedMemoryEntry {
                    id: PinnedMemoryId::new(format!("memory_{}", entries.len() + 1))
                        .expect("valid generated id"),
                    category: draft.category,
                    content: draft.content,
                    attributes: draft.attributes,
                };
                entries.push(entry.clone());
                Ok(entry)
            })
        }

        fn update(
            &self,
            id: PinnedMemoryId,
            patch: PinnedMemoryPatch,
            cancellation: CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                let mut entries = self.entries.lock().expect("lock entries");
                let entry = entries
                    .iter_mut()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| PinnedMemoryStoreError::NotFound { id: id.clone() })?;
                if let Some(category) = patch.category {
                    entry.category = category;
                }
                if let Some(content) = patch.content {
                    entry.content = content;
                }
                if let Some(attributes) = patch.attributes {
                    entry.attributes = attributes;
                }
                Ok(entry.clone())
            })
        }

        fn unpin(
            &self,
            id: PinnedMemoryId,
            cancellation: CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                let mut entries = self.entries.lock().expect("lock entries");
                let index = entries
                    .iter()
                    .position(|entry| entry.id == id)
                    .ok_or(PinnedMemoryStoreError::NotFound { id })?;
                Ok(entries.remove(index))
            })
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        use std::task::{Context, Poll};

        use futures_util::task::noop_waker_ref;

        let waker = noop_waker_ref();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("mini store future must not pend"),
        }
    }

    fn draft() -> PinnedMemoryDraft {
        PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "Use dark mode".to_owned(),
            attributes: BTreeMap::from([(
                "scope".to_owned(),
                MemoryPropertyValue::String("desktop".to_owned()),
            )]),
        }
    }

    #[test]
    fn pinned_store_contract_is_replaceable_and_preserves_error_categories() {
        let store = MiniStore {
            entries: Mutex::new(vec![]),
        };
        let saved = block_on(store.pin(draft(), CancellationToken::new())).expect("pin entry");
        assert_eq!(saved.id.as_str(), "memory_1");
        assert_eq!(
            block_on(store.list(CancellationToken::new())).expect("list entries"),
            vec![saved.clone()]
        );

        let updated = block_on(store.update(
            saved.id.clone(),
            PinnedMemoryPatch {
                content: Some("Use light mode".to_owned()),
                ..PinnedMemoryPatch::default()
            },
            CancellationToken::new(),
        ))
        .expect("update entry");
        assert_eq!(updated.content, "Use light mode");
        assert_eq!(
            block_on(store.unpin(saved.id, CancellationToken::new())).expect("unpin entry"),
            updated
        );

        let missing = PinnedMemoryId::new("missing").expect("valid id");
        assert_eq!(
            block_on(store.unpin(missing.clone(), CancellationToken::new())),
            Err(PinnedMemoryStoreError::NotFound { id: missing })
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            block_on(store.list(cancellation)),
            Err(PinnedMemoryStoreError::Cancelled)
        );

        let categories = [
            PinnedMemoryStoreError::InvalidInput {
                message: "invalid".to_owned(),
            },
            PinnedMemoryStoreError::CapacityExceeded {
                message: "full".to_owned(),
            },
            PinnedMemoryStoreError::Corrupt {
                message: "bad version".to_owned(),
            },
            PinnedMemoryStoreError::Io {
                message: "write failed".to_owned(),
            },
        ];
        assert_eq!(categories.len(), 4);
    }
}
