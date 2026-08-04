//! Demo 私有的版本化 JSON RecallSource。

use std::{
    collections::{BTreeMap, HashSet},
    num::NonZeroUsize,
    path::PathBuf,
};

use agent_memory::{
    MemoryPropertyValue, RecallSource, RecallSourceError, RecallSourceFuture, RecallSourceId,
    RecallSourceItem, RecallSourceRequest, RecallSourceResponse,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::atomic_json::{self, AtomicJsonError};

const RECALL_FILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DemoRecallRecord {
    pub(crate) reference: String,
    pub(crate) content: String,
    pub(crate) attributes: BTreeMap<String, MemoryPropertyValue>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DemoRecallFile {
    pub(crate) version: u32,
    pub(crate) records: Vec<DemoRecallRecord>,
}

impl DemoRecallFile {
    pub(crate) fn new(records: Vec<DemoRecallRecord>) -> Self {
        Self {
            version: RECALL_FILE_VERSION,
            records,
        }
    }
}

/// 每次调用都重新读取 JSON，允许开发者在不重启 Demo 的情况下修改样例数据。
pub(crate) struct DemoRecallSource {
    id: RecallSourceId,
    path: PathBuf,
    max_record_bytes: NonZeroUsize,
}

impl DemoRecallSource {
    pub(crate) fn new(id: RecallSourceId, path: PathBuf, max_record_bytes: NonZeroUsize) -> Self {
        Self {
            id,
            path,
            max_record_bytes,
        }
    }
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
                .map_err(map_read_error)?
                .ok_or_else(|| RecallSourceError::Io {
                    message: "recall data file does not exist".to_owned(),
                })?;
            if cancellation.is_cancelled() {
                return Err(RecallSourceError::Cancelled);
            }
            validate_file(&file, self.max_record_bytes)?;

            let mut matches = file
                .records
                .into_iter()
                .filter_map(|record| {
                    let haystack = searchable_text(&record);
                    let hits = terms
                        .iter()
                        .filter(|term| haystack.contains(term.as_str()))
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

fn normalized_terms(query: &str) -> Result<Vec<String>, RecallSourceError> {
    let mut seen = HashSet::new();
    let terms = query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .filter(|term| seen.insert(term.clone()))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(RecallSourceError::InvalidData {
            message: "recall query must contain at least one term".to_owned(),
        });
    }
    Ok(terms)
}

fn validate_file(
    file: &DemoRecallFile,
    max_record_bytes: NonZeroUsize,
) -> Result<(), RecallSourceError> {
    if file.version != RECALL_FILE_VERSION {
        return Err(invalid_data("unsupported recall file version"));
    }
    let mut references = HashSet::new();
    for record in &file.records {
        if record.reference.trim().is_empty() || record.reference.chars().any(char::is_control) {
            return Err(invalid_data("recall record has an invalid reference"));
        }
        if record.content.trim().is_empty()
            || record
                .content
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(invalid_data("recall record has invalid content"));
        }
        if !references.insert(record.reference.clone()) {
            return Err(invalid_data("recall file contains a duplicate reference"));
        }
        let actual_bytes = serde_json::to_vec(record)
            .map_err(|_| invalid_data("recall record cannot be serialized"))?
            .len();
        if actual_bytes > max_record_bytes.get() {
            return Err(invalid_data(
                "recall record exceeds the configured byte limit",
            ));
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

fn map_read_error(error: AtomicJsonError) -> RecallSourceError {
    match error {
        AtomicJsonError::InvalidData(_) => invalid_data("recall JSON is invalid"),
        AtomicJsonError::Io(message) => RecallSourceError::Io { message },
    }
}

fn invalid_data(message: &str) -> RecallSourceError {
    RecallSourceError::InvalidData {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn record(reference: &str, content: &str, scope: &str) -> DemoRecallRecord {
        DemoRecallRecord {
            reference: reference.to_owned(),
            content: content.to_owned(),
            attributes: BTreeMap::from([(
                "scope".to_owned(),
                MemoryPropertyValue::String(scope.to_owned()),
            )]),
        }
    }

    async fn write_file(path: &std::path::Path, file: &DemoRecallFile) {
        crate::atomic_json::AtomicJsonWriter::default()
            .write(path, file)
            .await
            .expect("write recall file");
    }

    fn source(path: PathBuf) -> DemoRecallSource {
        DemoRecallSource::new(
            RecallSourceId::new("demo_records").expect("valid source id"),
            path,
            NonZeroUsize::new(1024).expect("non-zero"),
        )
    }

    #[tokio::test]
    async fn recall_matches_content_reference_and_attributes_with_stable_ranking() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("recall-records.json");
        write_file(
            &path,
            &DemoRecallFile::new(vec![
                record("ref-b", "Rust project notes", "desktop"),
                record("ref-a", "Rust desktop preferences", "mobile"),
                record("ref-c", "Unrelated note", "desktop"),
            ]),
        )
        .await;
        let result = source(path)
            .recall(
                RecallSourceRequest {
                    query: "rust desktop".to_owned(),
                    limit: NonZeroUsize::new(2).expect("non-zero"),
                },
                CancellationToken::new(),
            )
            .await
            .expect("recall records");
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.reference.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("ref-a"), Some("ref-b")]
        );
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn recall_reloads_file_on_every_call_and_handles_empty_results() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("recall-records.json");
        write_file(
            &path,
            &DemoRecallFile::new(vec![record("old", "alpha", "demo")]),
        )
        .await;
        let source = source(path.clone());
        let first = source
            .recall(
                RecallSourceRequest {
                    query: "alpha".to_owned(),
                    limit: NonZeroUsize::new(4).expect("non-zero"),
                },
                CancellationToken::new(),
            )
            .await
            .expect("first recall");
        assert_eq!(first.items.len(), 1);

        write_file(
            &path,
            &DemoRecallFile::new(vec![record("new", "beta", "demo")]),
        )
        .await;
        let empty = source
            .recall(
                RecallSourceRequest {
                    query: "alpha".to_owned(),
                    limit: NonZeroUsize::new(4).expect("non-zero"),
                },
                CancellationToken::new(),
            )
            .await
            .expect("second recall");
        assert!(empty.items.is_empty());
    }

    #[tokio::test]
    async fn recall_rejects_malformed_version_duplicate_and_oversized_records() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("recall-records.json");
        fs::write(&path, b"{").expect("write malformed file");
        let source = source(path.clone());
        assert!(matches!(
            source
                .recall(
                    RecallSourceRequest {
                        query: "alpha".to_owned(),
                        limit: NonZeroUsize::new(1).expect("non-zero"),
                    },
                    CancellationToken::new(),
                )
                .await,
            Err(RecallSourceError::InvalidData { .. })
        ));

        write_file(
            &path,
            &DemoRecallFile {
                version: 2,
                records: vec![],
            },
        )
        .await;
        assert!(matches!(
            source
                .recall(
                    RecallSourceRequest {
                        query: "alpha".to_owned(),
                        limit: NonZeroUsize::new(1).expect("non-zero"),
                    },
                    CancellationToken::new(),
                )
                .await,
            Err(RecallSourceError::InvalidData { .. })
        ));

        write_file(
            &path,
            &DemoRecallFile::new(vec![
                record("duplicate", "alpha", "demo"),
                record("duplicate", "beta", "demo"),
            ]),
        )
        .await;
        assert!(matches!(
            source
                .recall(
                    RecallSourceRequest {
                        query: "alpha".to_owned(),
                        limit: NonZeroUsize::new(1).expect("non-zero"),
                    },
                    CancellationToken::new(),
                )
                .await,
            Err(RecallSourceError::InvalidData { .. })
        ));

        let tiny = DemoRecallSource::new(
            RecallSourceId::new("demo_records").expect("valid source id"),
            path.clone(),
            NonZeroUsize::new(8).expect("non-zero"),
        );
        write_file(
            &path,
            &DemoRecallFile::new(vec![record("same", "alpha", "demo")]),
        )
        .await;
        assert!(matches!(
            tiny.recall(
                RecallSourceRequest {
                    query: "alpha".to_owned(),
                    limit: NonZeroUsize::new(1).expect("non-zero"),
                },
                CancellationToken::new(),
            )
            .await,
            Err(RecallSourceError::InvalidData { .. })
        ));
    }
}
