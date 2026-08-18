//! Pinned Memory Store、RecallSource 与统一 MemoryRecall 的确定性 Fake。
//!
//! 所有状态只存在内存中；方法完整观察取消信号并记录类型化请求，不读取文件或网络。

use std::sync::Mutex;

use agent_memory::{
    MemoryRecall, MemoryRecallError, MemoryRecallFuture, MemoryRecallRequest, MemoryRecallResponse,
    PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryFuture, PinnedMemoryId, PinnedMemoryPatch,
    PinnedMemoryStore, PinnedMemoryStoreError, RecallReferenceReadFuture,
    RecallReferenceReadRequest, RecallReferenceReader, RecallSource, RecallSourceError,
    RecallSourceFuture, RecallSourceId, RecallSourceRequest, RecallSourceResponse,
};
use tokio_util::sync::CancellationToken;

/// Fake Pinned Store 记录的一次能力调用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedMemoryObservation {
    /// 读取最新完整状态。
    List,
    /// 请求 Store 分配 ID 并新增条目。
    Pin(PinnedMemoryDraft),
    /// 请求修改稳定 ID 对应的条目。
    Update {
        /// 目标稳定 ID。
        id: PinnedMemoryId,
        /// 要应用的部分修改。
        patch: PinnedMemoryPatch,
    },
    /// 请求删除稳定 ID 对应的条目。
    Unpin(PinnedMemoryId),
}

/// 可观察、可注入统一失败的内存 Pinned Memory Store。
pub struct FakePinnedMemoryStore {
    state: Mutex<FakePinnedMemoryState>,
    error: Option<PinnedMemoryStoreError>,
}

struct FakePinnedMemoryState {
    entries: Vec<PinnedMemoryEntry>,
    observations: Vec<PinnedMemoryObservation>,
    next_id: u64,
}

impl FakePinnedMemoryStore {
    /// 用给定权威条目创建正常 Fake Store。
    pub fn new(entries: Vec<PinnedMemoryEntry>) -> Self {
        Self {
            state: Mutex::new(FakePinnedMemoryState {
                next_id: entries.len() as u64 + 1,
                entries,
                observations: Vec::new(),
            }),
            error: None,
        }
    }

    /// 创建所有方法都返回同一个稳定错误的 Fake Store。
    pub fn failing(error: PinnedMemoryStoreError) -> Self {
        Self {
            state: Mutex::new(FakePinnedMemoryState {
                entries: Vec::new(),
                observations: Vec::new(),
                next_id: 1,
            }),
            error: Some(error),
        }
    }

    /// 返回当前权威条目快照。
    pub fn entries(&self) -> Vec<PinnedMemoryEntry> {
        self.state
            .lock()
            .expect("memory state poisoned")
            .entries
            .clone()
    }

    /// 返回按调用顺序记录的全部观察。
    pub fn observations(&self) -> Vec<PinnedMemoryObservation> {
        self.state
            .lock()
            .expect("memory state poisoned")
            .observations
            .clone()
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), PinnedMemoryStoreError> {
        if cancellation.is_cancelled() {
            return Err(PinnedMemoryStoreError::Cancelled);
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(())
    }
}

impl PinnedMemoryStore for FakePinnedMemoryStore {
    fn list(
        &self,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
        Box::pin(async move {
            self.check(&cancellation)?;
            let mut state = self.state.lock().expect("memory state poisoned");
            state.observations.push(PinnedMemoryObservation::List);
            Ok(state.entries.clone())
        })
    }

    fn pin(
        &self,
        draft: PinnedMemoryDraft,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            self.check(&cancellation)?;
            let mut state = self.state.lock().expect("memory state poisoned");
            state
                .observations
                .push(PinnedMemoryObservation::Pin(draft.clone()));
            let id = PinnedMemoryId::new(format!("fake_pinned_{}", state.next_id))
                .expect("generated fake ID is valid");
            state.next_id += 1;
            let entry = PinnedMemoryEntry {
                id,
                category: draft.category,
                content: draft.content,
                attributes: draft.attributes,
            };
            state.entries.push(entry.clone());
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
            self.check(&cancellation)?;
            let mut state = self.state.lock().expect("memory state poisoned");
            state.observations.push(PinnedMemoryObservation::Update {
                id: id.clone(),
                patch: patch.clone(),
            });
            let entry = state
                .entries
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
            self.check(&cancellation)?;
            let mut state = self.state.lock().expect("memory state poisoned");
            state
                .observations
                .push(PinnedMemoryObservation::Unpin(id.clone()));
            let index = state
                .entries
                .iter()
                .position(|entry| entry.id == id)
                .ok_or(PinnedMemoryStoreError::NotFound { id })?;
            Ok(state.entries.remove(index))
        })
    }
}

/// 返回固定结果并记录请求的单 Source Fake。
pub struct ScriptedRecallSource {
    id: RecallSourceId,
    result: Result<RecallSourceResponse, RecallSourceError>,
    requests: Mutex<Vec<RecallSourceRequest>>,
}

impl ScriptedRecallSource {
    /// 创建返回固定成功或失败结果的 Source。
    pub fn new(
        id: RecallSourceId,
        result: Result<RecallSourceResponse, RecallSourceError>,
    ) -> Self {
        Self {
            id,
            result,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 返回按调用顺序记录的全部 Source 请求。
    pub fn requests(&self) -> Vec<RecallSourceRequest> {
        self.requests
            .lock()
            .expect("recall source requests poisoned")
            .clone()
    }
}

impl RecallSource for ScriptedRecallSource {
    fn id(&self) -> &RecallSourceId {
        &self.id
    }

    fn recall(
        &self,
        request: RecallSourceRequest,
        cancellation: CancellationToken,
    ) -> RecallSourceFuture<'_, RecallSourceResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(RecallSourceError::Cancelled);
            }
            self.requests
                .lock()
                .expect("recall source requests poisoned")
                .push(request);
            self.result.clone()
        })
    }
}

/// 返回固定结果并记录请求的统一 MemoryRecall Fake。
pub struct ScriptedMemoryRecall {
    result: Result<MemoryRecallResponse, MemoryRecallError>,
    requests: Mutex<Vec<MemoryRecallRequest>>,
    read_requests: Mutex<Vec<RecallReferenceReadRequest>>,
}

impl ScriptedMemoryRecall {
    /// 创建返回固定成功或失败结果的统一召回能力。
    pub fn new(result: Result<MemoryRecallResponse, MemoryRecallError>) -> Self {
        Self {
            result,
            requests: Mutex::new(Vec::new()),
            read_requests: Mutex::new(Vec::new()),
        }
    }

    /// 返回按调用顺序记录的全部统一请求。
    pub fn requests(&self) -> Vec<MemoryRecallRequest> {
        self.requests
            .lock()
            .expect("memory recall requests poisoned")
            .clone()
    }

    /// 返回按调用顺序记录的全部稳定引用续读请求。
    pub fn read_requests(&self) -> Vec<RecallReferenceReadRequest> {
        self.read_requests
            .lock()
            .expect("recall read requests poisoned")
            .clone()
    }
}

impl RecallReferenceReader for ScriptedMemoryRecall {
    fn read_reference(
        &self,
        request: RecallReferenceReadRequest,
        cancellation: CancellationToken,
    ) -> RecallReferenceReadFuture<'_, MemoryRecallResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(MemoryRecallError::Cancelled);
            }
            self.read_requests
                .lock()
                .expect("recall read requests poisoned")
                .push(request);
            self.result.clone()
        })
    }
}

impl MemoryRecall for ScriptedMemoryRecall {
    fn recall(
        &self,
        request: MemoryRecallRequest,
        cancellation: CancellationToken,
    ) -> MemoryRecallFuture<'_, MemoryRecallResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(MemoryRecallError::Cancelled);
            }
            self.requests
                .lock()
                .expect("memory recall requests poisoned")
                .push(request);
            self.result.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use agent_memory::{
        MemoryPropertyValue, PinnedMemoryCategory, RecallItem, RecallOrigin, RecallSourceItem,
    };

    use super::*;

    fn entry() -> PinnedMemoryEntry {
        PinnedMemoryEntry {
            id: PinnedMemoryId::new("pinned_1").expect("valid id"),
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "Use dark mode".to_owned(),
            attributes: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn fake_pinned_store_mutates_and_observes_typed_operations() {
        let store = FakePinnedMemoryStore::new(vec![entry()]);
        let updated = store
            .update(
                PinnedMemoryId::new("pinned_1").expect("valid id"),
                PinnedMemoryPatch {
                    content: Some("Use light mode".to_owned()),
                    ..PinnedMemoryPatch::default()
                },
                CancellationToken::new(),
            )
            .await
            .expect("update entry");
        assert_eq!(updated.content, "Use light mode");
        assert!(matches!(
            store.observations().as_slice(),
            [PinnedMemoryObservation::Update { .. }]
        ));
    }

    #[tokio::test]
    async fn scripted_recall_fakes_return_results_and_record_requests() {
        let source_id = RecallSourceId::new("notes").expect("valid source id");
        let source = ScriptedRecallSource::new(
            source_id.clone(),
            Ok(RecallSourceResponse {
                items: vec![RecallSourceItem {
                    content: "remembered".to_owned(),
                    attributes: BTreeMap::new(),
                    reference: None,
                }],
                truncated: false,
            }),
        );
        source
            .recall(
                RecallSourceRequest {
                    query: "query".to_owned(),
                    limit: std::num::NonZeroUsize::new(1).expect("non-zero"),
                },
                CancellationToken::new(),
            )
            .await
            .expect("source recall");
        assert_eq!(source.requests().len(), 1);

        let recall = ScriptedMemoryRecall::new(Ok(MemoryRecallResponse {
            items: vec![RecallItem {
                content: "remembered".to_owned(),
                origins: vec![RecallOrigin {
                    source_id,
                    reference: None,
                }],
                attributes: BTreeMap::from([(
                    "rank".to_owned(),
                    MemoryPropertyValue::Number(serde_json::Number::from(1)),
                )]),
            }],
            failures: vec![],
            truncated: false,
            window: None,
        }));
        recall
            .recall(
                MemoryRecallRequest {
                    query: "query".to_owned(),
                    scope: agent_memory::RecallScope::Session,
                    limit: std::num::NonZeroUsize::new(1).expect("non-zero"),
                    sources: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect("memory recall");
        assert_eq!(recall.requests().len(), 1);
    }
}
