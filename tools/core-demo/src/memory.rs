//! Core Demo 私有的本地记忆 Store、RecallSource 与装配资源。

use std::{
    collections::{BTreeMap, HashSet},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use agent_memory::{
    CoordinatedMemoryRecall, CoordinatedMemoryRecallConfig, MemoryPropertyValue, MemoryRecall,
    PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryFuture, PinnedMemoryId, PinnedMemoryLimits,
    PinnedMemoryPatch, PinnedMemoryStore, PinnedMemoryStoreError, PinnedMemoryValidationError,
    RecallSource, RecallSourceError, RecallSourceFuture, RecallSourceId, RecallSourceItem,
    RecallSourceRequest, RecallSourceResponse,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::atomic_json::{self, AtomicJsonError, AtomicJsonWriter};

pub(crate) const PINNED_FILE: &str = "pinned-memory.json";
pub(crate) const RECALL_FILE: &str = "recall-records.json";
pub(crate) const DEMO_SOURCE_ID: &str = "demo_records";
pub(crate) const FAILING_SOURCE_ID: &str = "failing_demo";

const STORE_VERSION: u32 = 1;
const RECALL_FILE_VERSION: u32 = 1;
const ID_PREFIX: &str = "pinned_";

/// Core Demo 运行期间共享的记忆能力；具体文件格式仍是 Demo 私有实现。
pub(crate) struct DemoMemoryResources {
    pub(crate) store: Arc<DemoPinnedMemoryStore>,
    pub(crate) recall: Arc<dyn MemoryRecall>,
    pub(crate) limits: PinnedMemoryLimits,
}

/// UI 和新 Session 构建使用的 Store 一致性快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DemoPinnedStoreSnapshot {
    pub(crate) revision: u64,
    pub(crate) entries: Vec<PinnedMemoryEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PinnedStoreFile {
    version: u32,
    revision: u64,
    next_id: u64,
    entries: Vec<PinnedMemoryEntry>,
}

impl Default for PinnedStoreFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            revision: 0,
            next_id: 1,
            entries: vec![],
        }
    }
}

/// 只供 Core Demo 装配的版本化 JSON Pinned Store。
pub(crate) struct DemoPinnedMemoryStore {
    path: PathBuf,
    limits: PinnedMemoryLimits,
    writer: AtomicJsonWriter,
    state: Mutex<PinnedStoreFile>,
}

impl DemoPinnedMemoryStore {
    async fn open(
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

    /// 同时读取 revision 与 entries，避免 Session 构建观察到拆分状态。
    pub(crate) async fn snapshot(&self) -> DemoPinnedStoreSnapshot {
        let state = self.state.lock().await;
        DemoPinnedStoreSnapshot {
            revision: state.revision,
            entries: state.entries.clone(),
        }
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
            let entry = PinnedMemoryEntry {
                id: generated_id(state.next_id)?,
                category: draft.category,
                content: draft.content,
                attributes: draft.attributes,
            };
            entry.validate(&self.limits).map_err(map_validation_error)?;

            let mut candidate = state.clone();
            candidate.next_id = candidate.next_id.checked_add(1).ok_or_else(|| {
                PinnedMemoryStoreError::CapacityExceeded {
                    message: "pinned memory id sequence is exhausted".to_owned(),
                }
            })?;
            candidate.revision = candidate.revision.checked_add(1).ok_or_else(|| {
                PinnedMemoryStoreError::CapacityExceeded {
                    message: "pinned memory revision is exhausted".to_owned(),
                }
            })?;
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
            candidate.revision = candidate.revision.checked_add(1).ok_or_else(|| {
                PinnedMemoryStoreError::CapacityExceeded {
                    message: "pinned memory revision is exhausted".to_owned(),
                }
            })?;
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
            candidate.revision = candidate.revision.checked_add(1).ok_or_else(|| {
                PinnedMemoryStoreError::CapacityExceeded {
                    message: "pinned memory revision is exhausted".to_owned(),
                }
            })?;
            self.persist_candidate(&candidate).await?;
            *state = candidate;
            Ok(removed)
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DemoRecallRecord {
    reference: String,
    content: String,
    attributes: BTreeMap<String, MemoryPropertyValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DemoRecallFile {
    version: u32,
    records: Vec<DemoRecallRecord>,
}

/// 每次调用都重新读取文件，便于开发者直接修改样例记录进行验证。
struct DemoRecallSource {
    id: RecallSourceId,
    path: PathBuf,
    max_record_bytes: NonZeroUsize,
}

impl RecallSource for DemoRecallSource {
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
            let terms = normalized_terms(&request.query)?;
            let file = atomic_json::read::<DemoRecallFile>(&self.path)
                .await
                .map_err(map_recall_read_error)?
                .ok_or_else(|| RecallSourceError::Io {
                    message: "recall data file does not exist".to_owned(),
                })?;
            validate_recall_file(&file, self.max_record_bytes)?;

            let mut matches = file
                .records
                .into_iter()
                .filter_map(|record| {
                    let hits = terms
                        .iter()
                        .filter(|term| searchable_text(&record).contains(term.as_str()))
                        .count();
                    (hits > 0).then_some((hits, record))
                })
                .collect::<Vec<_>>();
            matches.sort_by(|(left_hits, left), (right_hits, right)| {
                right_hits
                    .cmp(left_hits)
                    .then_with(|| left.reference.cmp(&right.reference))
            });
            let truncated = matches.len() > request.limit.get();
            let items = matches
                .into_iter()
                .take(request.limit.get())
                .map(|(_, record)| RecallSourceItem {
                    content: record.content,
                    attributes: record.attributes,
                    reference: Some(record.reference),
                })
                .collect();
            Ok(RecallSourceResponse { items, truncated })
        })
    }
}

struct FailingDemoSource {
    id: RecallSourceId,
}

impl RecallSource for FailingDemoSource {
    fn id(&self) -> &RecallSourceId {
        &self.id
    }

    fn recall(
        &self,
        _request: RecallSourceRequest,
        cancellation: CancellationToken,
    ) -> RecallSourceFuture<'_, RecallSourceResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                Err(RecallSourceError::Cancelled)
            } else {
                Err(RecallSourceError::Unavailable {
                    message: "intentional demo source failure".to_owned(),
                })
            }
        })
    }
}

/// 创建 Demo 私有文件并装配统一记忆能力。
pub(crate) async fn build_memory_resources(
    data_dir: &Path,
) -> Result<DemoMemoryResources, DemoMemoryError> {
    tokio::fs::create_dir_all(data_dir)
        .await
        .map_err(|error| DemoMemoryError::Io(error.to_string()))?;
    initialize_recall_file(&data_dir.join(RECALL_FILE)).await?;

    let limits = pinned_limits();
    let store = DemoPinnedMemoryStore::open(data_dir.join(PINNED_FILE), limits.clone())
        .await
        .map_err(|error| DemoMemoryError::Store(error.to_string()))?;
    let demo_source_id = RecallSourceId::new(DEMO_SOURCE_ID)
        .map_err(|error| DemoMemoryError::Recall(error.to_string()))?;
    let failing_source_id = RecallSourceId::new(FAILING_SOURCE_ID)
        .map_err(|error| DemoMemoryError::Recall(error.to_string()))?;
    let sources: Vec<Arc<dyn RecallSource>> = vec![
        Arc::new(DemoRecallSource {
            id: demo_source_id.clone(),
            path: data_dir.join(RECALL_FILE),
            max_record_bytes: non_zero(4096),
        }),
        Arc::new(FailingDemoSource {
            id: failing_source_id,
        }),
    ];
    let recall: Arc<dyn MemoryRecall> = Arc::new(
        CoordinatedMemoryRecall::new(
            sources,
            CoordinatedMemoryRecallConfig {
                default_sources: vec![demo_source_id],
                source_timeout: Duration::from_secs(5),
                max_sources: non_zero(2),
                max_query_bytes: non_zero(1024),
                max_source_id_bytes: non_zero(64),
                max_item_bytes: non_zero(4096),
            },
        )
        .map_err(|error| DemoMemoryError::Recall(error.to_string()))?,
    );
    Ok(DemoMemoryResources {
        store,
        recall,
        limits,
    })
}

fn pinned_limits() -> PinnedMemoryLimits {
    PinnedMemoryLimits {
        max_entries: non_zero(64),
        max_id_bytes: non_zero(32),
        max_category_bytes: non_zero(64),
        max_content_bytes: non_zero(4096),
        max_attributes_per_entry: non_zero(16),
        max_attribute_key_bytes: non_zero(64),
        max_attribute_string_bytes: non_zero(512),
        max_description_bytes: non_zero(1024),
        max_snapshot_bytes: non_zero(256 * 1024),
    }
}

async fn initialize_recall_file(path: &Path) -> Result<(), DemoMemoryError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| DemoMemoryError::Io(error.to_string()))?
    {
        return Ok(());
    }
    let file = DemoRecallFile {
        version: RECALL_FILE_VERSION,
        records: vec![
            DemoRecallRecord {
                reference: "demo://project/architecture".to_owned(),
                content: "ez-assistant is a local-first desktop AI assistant whose Agent Core is independent from the application Runtime.".to_owned(),
                attributes: BTreeMap::from([(
                    "kind".to_owned(),
                    MemoryPropertyValue::String("project_note".to_owned()),
                )]),
            },
            DemoRecallRecord {
                reference: "demo://meeting/memory-design".to_owned(),
                content: "Pinned memory belongs in a frozen system prompt snapshot, while larger historical knowledge is queried through recall_memory.".to_owned(),
                attributes: BTreeMap::from([(
                    "kind".to_owned(),
                    MemoryPropertyValue::String("meeting_note".to_owned()),
                )]),
            },
        ],
    };
    AtomicJsonWriter::default()
        .write(path, &file)
        .await
        .map_err(|error| DemoMemoryError::Io(error.to_string()))
}

fn validate_loaded_state(
    state: &PinnedStoreFile,
    limits: &PinnedMemoryLimits,
) -> Result<(), PinnedMemoryStoreError> {
    if state.version != STORE_VERSION || state.next_id == 0 {
        return Err(corrupt("unsupported or invalid pinned memory file header"));
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
        maximum_sequence = maximum_sequence.max(
            parse_generated_id(&entry.id)
                .ok_or_else(|| corrupt("pinned memory file contains an invalid demo id"))?,
        );
    }
    if state.next_id <= maximum_sequence || state.revision < state.entries.len() as u64 {
        return Err(corrupt(
            "pinned memory sequence or revision is inconsistent",
        ));
    }
    Ok(())
}

fn generated_id(sequence: u64) -> Result<PinnedMemoryId, PinnedMemoryStoreError> {
    PinnedMemoryId::new(format!("{ID_PREFIX}{sequence:010}"))
        .map_err(|_| corrupt("generated pinned memory id is invalid"))
}

fn parse_generated_id(id: &PinnedMemoryId) -> Option<u64> {
    let digits = id.as_str().strip_prefix(ID_PREFIX)?;
    if digits.len() != 10 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
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

fn normalized_terms(query: &str) -> Result<Vec<String>, RecallSourceError> {
    let mut seen = HashSet::new();
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| seen.insert(term.clone()))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        Err(RecallSourceError::InvalidData {
            message: "recall query must contain at least one term".to_owned(),
        })
    } else {
        Ok(terms)
    }
}

fn validate_recall_file(
    file: &DemoRecallFile,
    max_record_bytes: NonZeroUsize,
) -> Result<(), RecallSourceError> {
    if file.version != RECALL_FILE_VERSION {
        return Err(invalid_recall("unsupported recall file version"));
    }
    let mut references = HashSet::new();
    for record in &file.records {
        if record.reference.trim().is_empty() || record.content.trim().is_empty() {
            return Err(invalid_recall("recall record has blank content"));
        }
        if !references.insert(record.reference.clone()) {
            return Err(invalid_recall("recall file contains a duplicate reference"));
        }
        if serde_json::to_vec(record)
            .map_err(|_| invalid_recall("recall record cannot be serialized"))?
            .len()
            > max_record_bytes.get()
        {
            return Err(invalid_recall("recall record exceeds the byte limit"));
        }
    }
    Ok(())
}

fn searchable_text(record: &DemoRecallRecord) -> String {
    let mut text = format!(
        "{}\n{}",
        record.reference.to_lowercase(),
        record.content.to_lowercase()
    );
    for (key, value) in &record.attributes {
        text.push('\n');
        text.push_str(&key.to_lowercase());
        text.push('=');
        match value {
            MemoryPropertyValue::String(value) => text.push_str(&value.to_lowercase()),
            MemoryPropertyValue::Number(value) => text.push_str(&value.to_string()),
        }
    }
    text
}

fn map_recall_read_error(error: AtomicJsonError) -> RecallSourceError {
    match error {
        AtomicJsonError::InvalidData(_) => invalid_recall("recall JSON is invalid"),
        AtomicJsonError::Io(message) => RecallSourceError::Io { message },
    }
}

fn invalid_recall(message: &str) -> RecallSourceError {
    RecallSourceError::InvalidData {
        message: message.to_owned(),
    }
}

fn non_zero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("static Demo limit is non-zero")
}

#[derive(Debug, Error)]
pub(crate) enum DemoMemoryError {
    #[error("memory file I/O failed: {0}")]
    Io(String),
    #[error("pinned memory Store failed: {0}")]
    Store(String),
    #[error("memory Recall failed: {0}")]
    Recall(String),
}

#[cfg(test)]
mod tests {
    use agent_memory::{MemoryRecallRequest, PinnedMemoryCategory, RecallFailureKind};

    use super::*;

    fn draft(content: &str) -> PinnedMemoryDraft {
        PinnedMemoryDraft {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: content.to_owned(),
            attributes: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn failed_persist_preserves_memory_and_authoritative_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join(PINNED_FILE);
        let limits = pinned_limits();
        let original_store = DemoPinnedMemoryStore::open(path.clone(), limits.clone())
            .await
            .expect("open store");
        let original = original_store
            .pin(draft("original"), CancellationToken::new())
            .await
            .expect("pin original");
        drop(original_store);

        let failing = DemoPinnedMemoryStore::open_with_writer(
            path.clone(),
            limits.clone(),
            AtomicJsonWriter::failing_before_persist(),
        )
        .await
        .expect("open failing store");
        assert!(
            failing
                .pin(draft("not persisted"), CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(failing.snapshot().await.entries, vec![original.clone()]);
        drop(failing);
        assert_eq!(
            DemoPinnedMemoryStore::open(path, limits)
                .await
                .expect("reopen store")
                .snapshot()
                .await
                .entries,
            vec![original]
        );
    }

    #[tokio::test]
    async fn recall_keeps_origin_and_partial_source_failure() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let resources = build_memory_resources(directory.path())
            .await
            .expect("memory resources");
        let response = resources
            .recall
            .recall(
                MemoryRecallRequest {
                    query: "memory".to_owned(),
                    limit: non_zero(4),
                    sources: Some(vec![
                        RecallSourceId::new(DEMO_SOURCE_ID).expect("demo source"),
                        RecallSourceId::new(FAILING_SOURCE_ID).expect("failing source"),
                    ]),
                },
                CancellationToken::new(),
            )
            .await
            .expect("partial result");
        assert!(!response.items.is_empty());
        assert_eq!(
            response.items[0].origins[0].source_id.as_str(),
            DEMO_SOURCE_ID
        );
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].kind, RecallFailureKind::Unavailable);
    }
}
