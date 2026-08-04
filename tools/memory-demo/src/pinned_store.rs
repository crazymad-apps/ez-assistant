//! Demo 私有的版本化 JSON Pinned Memory Store。

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use agent_memory::{
    PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryFuture, PinnedMemoryId, PinnedMemoryLimits,
    PinnedMemoryPatch, PinnedMemoryStore, PinnedMemoryStoreError, PinnedMemoryValidationError,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::atomic_json::{self, AtomicJsonError, AtomicJsonWriter};

const STORE_VERSION: u32 = 1;
const ID_PREFIX: &str = "pinned_";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PinnedStoreFile {
    version: u32,
    next_id: u64,
    entries: Vec<PinnedMemoryEntry>,
}

impl Default for PinnedStoreFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            next_id: 1,
            entries: vec![],
        }
    }
}

/// 只供 Memory Demo 装配的本地 Store；同一文件只允许一个实例作为 writer。
pub(crate) struct DemoPinnedMemoryStore {
    path: PathBuf,
    limits: PinnedMemoryLimits,
    writer: AtomicJsonWriter,
    state: Mutex<PinnedStoreFile>,
}

impl DemoPinnedMemoryStore {
    pub(crate) async fn open(
        path: PathBuf,
        limits: PinnedMemoryLimits,
    ) -> Result<Arc<Self>, PinnedMemoryStoreError> {
        Self::open_with_writer(path, limits, AtomicJsonWriter::default()).await
    }

    async fn open_with_writer(
        path: PathBuf,
        limits: PinnedMemoryLimits,
        writer: AtomicJsonWriter,
    ) -> Result<Arc<Self>, PinnedMemoryStoreError> {
        let mut state = atomic_json::read::<PinnedStoreFile>(&path)
            .await
            .map_err(map_open_error)?
            .unwrap_or_default();
        validate_loaded_state(&state, &limits)?;
        sort_entries(&mut state.entries);
        Ok(Arc::new(Self {
            path,
            limits,
            writer,
            state: Mutex::new(state),
        }))
    }

    #[cfg(test)]
    async fn open_with_failing_writer(
        path: PathBuf,
        limits: PinnedMemoryLimits,
    ) -> Result<Arc<Self>, PinnedMemoryStoreError> {
        Self::open_with_writer(path, limits, AtomicJsonWriter::failing_before_persist()).await
    }

    async fn persist_candidate(
        &self,
        candidate: &PinnedStoreFile,
    ) -> Result<(), PinnedMemoryStoreError> {
        self.writer
            .write(&self.path, candidate)
            .await
            .map_err(map_write_error)
    }
}

impl PinnedMemoryStore for DemoPinnedMemoryStore {
    fn list(
        &self,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            Ok(self.state.lock().await.entries.clone())
        })
    }

    fn pin(
        &self,
        draft: PinnedMemoryDraft,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            draft.validate(&self.limits).map_err(map_validation_error)?;
            let mut state = self.state.lock().await;
            ensure_not_cancelled(&cancellation)?;
            if state.entries.len() >= self.limits.max_entries.get() {
                return Err(PinnedMemoryStoreError::CapacityExceeded {
                    message: "maximum pinned memory entry count reached".to_owned(),
                });
            }
            let next_id = state.next_id.checked_add(1).ok_or_else(|| {
                PinnedMemoryStoreError::CapacityExceeded {
                    message: "pinned memory id sequence is exhausted".to_owned(),
                }
            })?;
            let entry = PinnedMemoryEntry {
                id: generated_id(state.next_id)?,
                category: draft.category,
                content: draft.content,
                attributes: draft.attributes,
            };
            entry.validate(&self.limits).map_err(map_validation_error)?;

            let mut candidate = state.clone();
            candidate.next_id = next_id;
            candidate.entries.push(entry.clone());
            sort_entries(&mut candidate.entries);
            self.persist_candidate(&candidate).await?;
            *state = candidate;
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
            ensure_not_cancelled(&cancellation)?;
            id.validate(&self.limits).map_err(map_validation_error)?;
            patch.validate(&self.limits).map_err(map_validation_error)?;
            let mut state = self.state.lock().await;
            ensure_not_cancelled(&cancellation)?;
            let mut candidate = state.clone();
            let entry = candidate
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
            entry.validate(&self.limits).map_err(map_validation_error)?;
            let updated = entry.clone();
            self.persist_candidate(&candidate).await?;
            *state = candidate;
            Ok(updated)
        })
    }

    fn unpin(
        &self,
        id: PinnedMemoryId,
        cancellation: CancellationToken,
    ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
        Box::pin(async move {
            ensure_not_cancelled(&cancellation)?;
            id.validate(&self.limits).map_err(map_validation_error)?;
            let mut state = self.state.lock().await;
            ensure_not_cancelled(&cancellation)?;
            let mut candidate = state.clone();
            let index = candidate
                .entries
                .iter()
                .position(|entry| entry.id == id)
                .ok_or_else(|| PinnedMemoryStoreError::NotFound { id: id.clone() })?;
            let removed = candidate.entries.remove(index);
            self.persist_candidate(&candidate).await?;
            *state = candidate;
            Ok(removed)
        })
    }
}

fn validate_loaded_state(
    state: &PinnedStoreFile,
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryStoreError> {
    if state.version != STORE_VERSION {
        return Err(corrupt("unsupported pinned memory file version"));
    }
    if state.next_id == 0 {
        return Err(corrupt("next_id must be greater than zero"));
    }
    if state.entries.len() > limits.max_entries.get() {
        return Err(corrupt("pinned memory file exceeds the entry limit"));
    }

    let mut ids = HashSet::new();
    let mut maximum_sequence = 0;
    for entry in &state.entries {
        entry
            .validate(limits)
            .map_err(|_| corrupt("pinned memory file contains an invalid entry"))?;
        if !ids.insert(entry.id.clone()) {
            return Err(corrupt("pinned memory file contains a duplicate id"));
        }
        let sequence = parse_generated_id(&entry.id)
            .ok_or_else(|| corrupt("pinned memory file contains an invalid demo id"))?;
        maximum_sequence = maximum_sequence.max(sequence);
    }
    if state.next_id <= maximum_sequence {
        return Err(corrupt("next_id does not follow the largest persisted id"));
    }
    Ok(())
}

fn generated_id(sequence: u64) -> Result<PinnedMemoryId, PinnedMemoryStoreError> {
    PinnedMemoryId::new(format!("{ID_PREFIX}{sequence:010}"))
        .map_err(|_| corrupt("generated pinned memory id is invalid"))
}

fn parse_generated_id(id: &PinnedMemoryId) -> Option<u64> {
    let digits = id.as_str().strip_prefix(ID_PREFIX)?;
    if digits.len() < 10 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let sequence = digits.parse::<u64>().ok()?;
    (sequence > 0 && generated_id(sequence).ok().as_ref() == Some(id)).then_some(sequence)
}

fn sort_entries(entries: &mut [PinnedMemoryEntry]) {
    entries.sort_by_key(|entry| parse_generated_id(&entry.id).unwrap_or(u64::MAX));
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), PinnedMemoryStoreError> {
    if cancellation.is_cancelled() {
        Err(PinnedMemoryStoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_validation_error(error: PinnedMemoryValidationError) -> PinnedMemoryStoreError {
    match error {
        PinnedMemoryValidationError::TooLong { .. }
        | PinnedMemoryValidationError::TooManyEntries { .. }
        | PinnedMemoryValidationError::TooManyAttributes { .. } => {
            PinnedMemoryStoreError::CapacityExceeded {
                message: error.to_string(),
            }
        }
        _ => PinnedMemoryStoreError::InvalidInput {
            message: error.to_string(),
        },
    }
}

fn map_open_error(error: AtomicJsonError) -> PinnedMemoryStoreError {
    match error {
        AtomicJsonError::InvalidData(_) => corrupt("pinned memory JSON is invalid"),
        AtomicJsonError::Io(message) => PinnedMemoryStoreError::Io { message },
    }
}

fn map_write_error(error: AtomicJsonError) -> PinnedMemoryStoreError {
    PinnedMemoryStoreError::Io {
        message: error.to_string(),
    }
}

fn corrupt(message: &str) -> PinnedMemoryStoreError {
    PinnedMemoryStoreError::Corrupt {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroUsize};

    use agent_memory::{MemoryPropertyValue, PinnedMemoryCategory};

    use super::*;

    fn limits() -> PinnedMemoryLimits {
        PinnedMemoryLimits {
            max_entries: NonZeroUsize::new(8).expect("non-zero"),
            max_id_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_category_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_content_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_attributes_per_entry: NonZeroUsize::new(8).expect("non-zero"),
            max_attribute_key_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_attribute_string_bytes: NonZeroUsize::new(128).expect("non-zero"),
            max_description_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_snapshot_bytes: NonZeroUsize::new(8192).expect("non-zero"),
        }
    }

    fn draft(content: &str) -> PinnedMemoryDraft {
        PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: content.to_owned(),
            attributes: BTreeMap::from([(
                "scope".to_owned(),
                MemoryPropertyValue::String("demo".to_owned()),
            )]),
        }
    }

    #[tokio::test]
    async fn pinned_store_persists_stable_ids_and_does_not_reuse_deleted_sequences() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pinned-memory.json");
        let store = DemoPinnedMemoryStore::open(path.clone(), limits())
            .await
            .expect("open empty store");
        assert!(!path.exists());

        let first = store
            .pin(draft("first"), CancellationToken::new())
            .await
            .expect("pin first");
        let second = store
            .pin(draft("second"), CancellationToken::new())
            .await
            .expect("pin second");
        assert_eq!(first.id.as_str(), "pinned_0000000001");
        assert_eq!(second.id.as_str(), "pinned_0000000002");
        let second = store
            .update(
                second.id,
                PinnedMemoryPatch {
                    content: Some("second updated".to_owned()),
                    ..PinnedMemoryPatch::default()
                },
                CancellationToken::new(),
            )
            .await
            .expect("update second");
        assert_eq!(second.content, "second updated");
        store
            .unpin(first.id, CancellationToken::new())
            .await
            .expect("unpin first");
        drop(store);

        let restarted = DemoPinnedMemoryStore::open(path, limits())
            .await
            .expect("reopen store");
        let third = restarted
            .pin(draft("third"), CancellationToken::new())
            .await
            .expect("pin third");
        assert_eq!(third.id.as_str(), "pinned_0000000003");
        assert_eq!(
            restarted
                .list(CancellationToken::new())
                .await
                .expect("list entries"),
            vec![second, third]
        );
    }

    #[tokio::test]
    async fn pinned_store_serializes_concurrent_writes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store =
            DemoPinnedMemoryStore::open(directory.path().join("pinned-memory.json"), limits())
                .await
                .expect("open store");
        let left = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.pin(draft("left"), CancellationToken::new()).await })
        };
        let right = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.pin(draft("right"), CancellationToken::new()).await })
        };
        left.await.expect("left task").expect("left pin");
        right.await.expect("right task").expect("right pin");
        let entries = store
            .list(CancellationToken::new())
            .await
            .expect("list entries");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id.as_str(), "pinned_0000000001");
        assert_eq!(entries[1].id.as_str(), "pinned_0000000002");
    }

    #[tokio::test]
    async fn failed_persist_preserves_memory_and_authoritative_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pinned-memory.json");
        let writer = DemoPinnedMemoryStore::open(path.clone(), limits())
            .await
            .expect("open writer");
        let original = writer
            .pin(draft("original"), CancellationToken::new())
            .await
            .expect("pin original");
        drop(writer);

        let failing = DemoPinnedMemoryStore::open_with_failing_writer(path.clone(), limits())
            .await
            .expect("open failing writer");
        assert!(
            failing
                .update(
                    original.id.clone(),
                    PinnedMemoryPatch {
                        content: Some("changed".to_owned()),
                        ..PinnedMemoryPatch::default()
                    },
                    CancellationToken::new(),
                )
                .await
                .is_err()
        );
        assert_eq!(
            failing
                .list(CancellationToken::new())
                .await
                .expect("list in-memory state"),
            vec![original.clone()]
        );
        drop(failing);
        assert_eq!(
            DemoPinnedMemoryStore::open(path, limits())
                .await
                .expect("reopen authoritative file")
                .list(CancellationToken::new())
                .await
                .expect("list file state"),
            vec![original]
        );
    }

    #[tokio::test]
    async fn corrupt_version_duplicate_ids_and_limits_fail_without_overwrite() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("pinned-memory.json");
        let duplicate = PinnedMemoryEntry {
            id: PinnedMemoryId::new("pinned_0000000001").expect("valid id"),
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "entry".to_owned(),
            attributes: BTreeMap::new(),
        };
        let invalid_version = PinnedStoreFile {
            version: 2,
            next_id: 2,
            entries: vec![],
        };
        let bytes = serde_json::to_vec_pretty(&invalid_version).expect("serialize invalid file");
        std::fs::write(&path, &bytes).expect("write invalid file");
        assert!(matches!(
            DemoPinnedMemoryStore::open(path.clone(), limits()).await,
            Err(PinnedMemoryStoreError::Corrupt { .. })
        ));
        assert_eq!(std::fs::read(&path).expect("read unchanged file"), bytes);

        let duplicate_ids = PinnedStoreFile {
            version: 1,
            next_id: 2,
            entries: vec![duplicate.clone(), duplicate.clone()],
        };
        let bytes = serde_json::to_vec_pretty(&duplicate_ids).expect("serialize duplicate file");
        std::fs::write(&path, &bytes).expect("write duplicate file");
        assert!(matches!(
            DemoPinnedMemoryStore::open(path.clone(), limits()).await,
            Err(PinnedMemoryStoreError::Corrupt { .. })
        ));
        assert_eq!(std::fs::read(&path).expect("read unchanged file"), bytes);

        let mut small_limits = limits();
        small_limits.max_entries = NonZeroUsize::new(1).expect("non-zero");
        let over_limit = PinnedStoreFile {
            version: 1,
            next_id: 3,
            entries: vec![
                duplicate,
                PinnedMemoryEntry {
                    id: PinnedMemoryId::new("pinned_0000000002").expect("valid id"),
                    category: PinnedMemoryCategory::new("preference").expect("valid category"),
                    content: "second".to_owned(),
                    attributes: BTreeMap::new(),
                },
            ],
        };
        let bytes = serde_json::to_vec_pretty(&over_limit).expect("serialize oversized file");
        std::fs::write(&path, &bytes).expect("write oversized file");
        assert!(matches!(
            DemoPinnedMemoryStore::open(path.clone(), small_limits).await,
            Err(PinnedMemoryStoreError::Corrupt { .. })
        ));
        assert_eq!(std::fs::read(&path).expect("read unchanged file"), bytes);

        let malformed = directory.path().join("malformed.json");
        std::fs::write(&malformed, b"{").expect("write malformed file");
        assert!(matches!(
            DemoPinnedMemoryStore::open(malformed, limits()).await,
            Err(PinnedMemoryStoreError::Corrupt { .. })
        ));
    }
}
