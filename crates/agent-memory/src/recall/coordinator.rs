use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::Duration};

use futures_util::future::join_all;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::MemoryPropertyValue;

use super::{
    MemoryRecall, MemoryRecallError, MemoryRecallFailure, MemoryRecallFuture, MemoryRecallRequest,
    MemoryRecallResponse, RecallItem, RecallOrigin, RecallSource, RecallSourceError,
    RecallSourceId, RecallSourceItem, RecallSourceRequest, RecallSourceResponse,
};

/// 多 Source 协调器的全部显式配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoordinatedMemoryRecallConfig {
    /// 请求省略 Source 时使用的非空默认集合。
    pub default_sources: Vec<RecallSourceId>,
    /// 单个 Source 的最大执行时间。
    pub source_timeout: Duration,
    /// 单次统一请求最多选择的 Source 数。
    pub max_sources: NonZeroUsize,
    /// query 的最大 UTF-8 字节数。
    pub max_query_bytes: NonZeroUsize,
    /// Source ID 的最大 UTF-8 字节数。
    pub max_source_id_bytes: NonZeroUsize,
    /// Source 单条候选的正文、属性和 reference 合计最大 UTF-8 字节数。
    ///
    /// 协调器附加的 Source ID 另受 `max_source_id_bytes` 约束，合并来源数受
    /// `max_sources` 约束。
    pub max_item_bytes: NonZeroUsize,
}

/// 多 Source 协调器无法构造。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CoordinatedMemoryRecallConfigError {
    /// 没有接入任何 Source；上层此时不应注册召回工具。
    #[error("coordinated memory recall requires at least one source")]
    NoSources,
    /// 两个 Source 使用了同一个稳定 ID。
    #[error("duplicate recall source id `{id}`")]
    DuplicateSource {
        /// 重复的 Source ID。
        id: RecallSourceId,
    },
    /// 默认 Source 集合为空。
    #[error("coordinated memory recall requires at least one default source")]
    NoDefaultSources,
    /// 默认 Source 集合包含重复 ID。
    #[error("duplicate default recall source id `{id}`")]
    DuplicateDefaultSource {
        /// 重复的默认 Source ID。
        id: RecallSourceId,
    },
    /// 默认集合引用了未接入的 Source。
    #[error("unknown default recall source id `{id}`")]
    UnknownDefaultSource {
        /// 未接入的默认 Source ID。
        id: RecallSourceId,
    },
    /// 默认集合超过允许的 Source 数量。
    #[error("default recall sources contain {actual} entries, exceeding the {max} source limit")]
    TooManyDefaultSources {
        /// 实际默认 Source 数量。
        actual: usize,
        /// 允许的最大数量。
        max: usize,
    },
    /// Source ID 超过显式容量上限。
    #[error("recall source id is {actual_bytes} bytes, exceeding the {max_bytes} byte limit")]
    SourceIdTooLong {
        /// 实际 UTF-8 字节数。
        actual_bytes: usize,
        /// 允许的最大 UTF-8 字节数。
        max_bytes: usize,
    },
    /// 单 Source 超时为零。
    #[error("recall source timeout must be greater than zero")]
    ZeroSourceTimeout,
}

/// `MemoryRecall` 的确定性多 Source 实现。
pub struct CoordinatedMemoryRecall {
    sources: Vec<Arc<dyn RecallSource>>,
    source_indices: BTreeMap<RecallSourceId, usize>,
    default_source_indices: Vec<usize>,
    config: CoordinatedMemoryRecallConfig,
}

impl CoordinatedMemoryRecall {
    /// 校验 Source 集合和显式配置并创建协调器。
    pub fn new(
        sources: Vec<Arc<dyn RecallSource>>,
        config: CoordinatedMemoryRecallConfig,
    ) -> Result<Self, CoordinatedMemoryRecallConfigError> {
        if sources.is_empty() {
            return Err(CoordinatedMemoryRecallConfigError::NoSources);
        }
        if config.default_sources.is_empty() {
            return Err(CoordinatedMemoryRecallConfigError::NoDefaultSources);
        }
        if config.source_timeout.is_zero() {
            return Err(CoordinatedMemoryRecallConfigError::ZeroSourceTimeout);
        }
        if config.default_sources.len() > config.max_sources.get() {
            return Err(CoordinatedMemoryRecallConfigError::TooManyDefaultSources {
                actual: config.default_sources.len(),
                max: config.max_sources.get(),
            });
        }

        let mut source_indices = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            validate_source_id_length(source.id(), config.max_source_id_bytes)?;
            if source_indices.insert(source.id().clone(), index).is_some() {
                return Err(CoordinatedMemoryRecallConfigError::DuplicateSource {
                    id: source.id().clone(),
                });
            }
        }

        let mut default_source_indices = Vec::with_capacity(config.default_sources.len());
        for source_id in &config.default_sources {
            validate_source_id_length(source_id, config.max_source_id_bytes)?;
            let index = source_indices.get(source_id).copied().ok_or_else(|| {
                CoordinatedMemoryRecallConfigError::UnknownDefaultSource {
                    id: source_id.clone(),
                }
            })?;
            if default_source_indices.contains(&index) {
                return Err(CoordinatedMemoryRecallConfigError::DuplicateDefaultSource {
                    id: source_id.clone(),
                });
            }
            default_source_indices.push(index);
        }
        default_source_indices.sort_unstable();

        Ok(Self {
            sources,
            source_indices,
            default_source_indices,
            config,
        })
    }

    fn selected_source_indices(
        &self,
        request: &MemoryRecallRequest,
    ) -> Result<Vec<usize>, MemoryRecallError> {
        validate_query(&request.query, self.config.max_query_bytes)?;

        let Some(source_ids) = &request.sources else {
            return Ok(self.default_source_indices.clone());
        };
        if source_ids.is_empty() {
            return Err(MemoryRecallError::invalid_input(
                "explicit recall source list must not be empty",
            ));
        }
        if source_ids.len() > self.config.max_sources.get() {
            return Err(MemoryRecallError::invalid_input(format!(
                "requested {} recall sources, exceeding the {} source limit",
                source_ids.len(),
                self.config.max_sources
            )));
        }

        let mut selected = Vec::with_capacity(source_ids.len());
        for source_id in source_ids {
            if source_id.as_str().len() > self.config.max_source_id_bytes.get() {
                return Err(MemoryRecallError::invalid_input(
                    "requested recall source id exceeds the configured byte limit",
                ));
            }
            let index = self.source_indices.get(source_id).copied().ok_or_else(|| {
                MemoryRecallError::invalid_input(format!("unknown recall source id `{source_id}`"))
            })?;
            if selected.contains(&index) {
                return Err(MemoryRecallError::invalid_input(format!(
                    "duplicate recall source id `{source_id}`"
                )));
            }
            selected.push(index);
        }
        selected.sort_unstable();
        Ok(selected)
    }

    async fn recall_selected(
        &self,
        request: MemoryRecallRequest,
        cancellation: CancellationToken,
    ) -> Result<MemoryRecallResponse, MemoryRecallError> {
        if cancellation.is_cancelled() {
            return Err(MemoryRecallError::Cancelled);
        }
        let selected = self.selected_source_indices(&request)?;
        if cancellation.is_cancelled() {
            return Err(MemoryRecallError::Cancelled);
        }

        let futures = selected.iter().map(|index| {
            let source = &self.sources[*index];
            let source_request = RecallSourceRequest {
                query: request.query.clone(),
                limit: request.limit,
            };
            run_source(
                source.as_ref(),
                source_request,
                cancellation.clone(),
                self.config.source_timeout,
                self.config.max_item_bytes,
            )
        });
        let outcomes = join_all(futures).await;
        if cancellation.is_cancelled() {
            return Err(MemoryRecallError::Cancelled);
        }

        let mut successes = Vec::new();
        let mut failures = Vec::new();
        let mut truncated = false;
        for (index, outcome) in selected.into_iter().zip(outcomes) {
            let source_id = self.sources[index].id().clone();
            match outcome {
                Ok(response) => {
                    truncated |= response.truncated;
                    successes.push((source_id, response.items));
                }
                Err(error) => {
                    let (kind, message) = error.into_failure_parts();
                    failures.push(MemoryRecallFailure {
                        source_id,
                        kind,
                        message,
                    });
                }
            }
        }

        if successes.is_empty() {
            return Err(MemoryRecallError::AllSourcesFailed { failures });
        }

        let mut items = merge_round_robin(successes);
        if items.len() > request.limit.get() {
            truncated = true;
            items.truncate(request.limit.get());
        }
        Ok(MemoryRecallResponse {
            items,
            failures,
            truncated,
        })
    }
}

impl MemoryRecall for CoordinatedMemoryRecall {
    fn recall(
        &self,
        request: MemoryRecallRequest,
        cancellation: CancellationToken,
    ) -> MemoryRecallFuture<'_, MemoryRecallResponse> {
        Box::pin(self.recall_selected(request, cancellation))
    }
}

async fn run_source(
    source: &dyn RecallSource,
    request: RecallSourceRequest,
    cancellation: CancellationToken,
    timeout: Duration,
    max_item_bytes: NonZeroUsize,
) -> Result<RecallSourceResponse, RecallSourceError> {
    let requested_limit = request.limit;
    let source_future = source.recall(request, cancellation.clone());
    let response = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(RecallSourceError::Cancelled),
        result = tokio::time::timeout(timeout, source_future) => match result {
            Ok(response) => response?,
            Err(_) => return Err(RecallSourceError::Timeout {
                message: "source exceeded the configured timeout".to_owned(),
            }),
        }
    };
    validate_source_response(&response, requested_limit, max_item_bytes)?;
    Ok(response)
}

fn validate_source_response(
    response: &RecallSourceResponse,
    requested_limit: NonZeroUsize,
    max_item_bytes: NonZeroUsize,
) -> Result<(), RecallSourceError> {
    if response.items.len() > requested_limit.get() {
        return Err(invalid_source_data(
            "source returned more candidates than requested",
        ));
    }
    for item in &response.items {
        validate_source_item(item, max_item_bytes)?;
    }
    Ok(())
}

fn validate_source_item(
    item: &RecallSourceItem,
    max_item_bytes: NonZeroUsize,
) -> Result<(), RecallSourceError> {
    validate_model_text("source item content", &item.content, true)?;
    let mut byte_count = item.content.len();
    for (key, value) in &item.attributes {
        validate_model_text("source item attribute key", key, false)?;
        byte_count = byte_count.saturating_add(key.len());
        match value {
            MemoryPropertyValue::String(value) => {
                validate_model_text("source item attribute string", value, true)?;
                byte_count = byte_count.saturating_add(value.len());
            }
            MemoryPropertyValue::Number(value) => {
                byte_count = byte_count.saturating_add(value.to_string().len());
            }
        }
    }
    if let Some(reference) = &item.reference {
        validate_model_text("source item reference", reference, false)?;
        byte_count = byte_count.saturating_add(reference.len());
    }
    if byte_count > max_item_bytes.get() {
        return Err(invalid_source_data(
            "source item exceeds the configured byte limit",
        ));
    }
    Ok(())
}

fn validate_model_text(
    field: &'static str,
    value: &str,
    allow_text_whitespace: bool,
) -> Result<(), RecallSourceError> {
    if value.trim().is_empty() {
        return Err(invalid_source_data(format!("{field} must not be blank")));
    }
    if value.chars().any(|character| {
        character.is_control()
            && !(allow_text_whitespace && matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(invalid_source_data(format!(
            "{field} contains a disallowed control character"
        )));
    }
    Ok(())
}

fn invalid_source_data(message: impl Into<String>) -> RecallSourceError {
    RecallSourceError::InvalidData {
        message: message.into(),
    }
}

fn merge_round_robin(sources: Vec<(RecallSourceId, Vec<RecallSourceItem>)>) -> Vec<RecallItem> {
    let max_rank = sources
        .iter()
        .map(|(_, items)| items.len())
        .max()
        .unwrap_or(0);
    let mut merged: Vec<RecallItem> = Vec::new();
    for rank in 0..max_rank {
        for (source_id, source_items) in &sources {
            let Some(candidate) = source_items.get(rank) else {
                continue;
            };
            let origin = RecallOrigin {
                source_id: source_id.clone(),
                reference: candidate.reference.clone(),
            };
            if let Some(existing) = merged.iter_mut().find(|item| {
                item.content == candidate.content && item.attributes == candidate.attributes
            }) {
                if !existing.origins.contains(&origin) {
                    existing.origins.push(origin);
                }
                continue;
            }
            merged.push(RecallItem {
                content: candidate.content.clone(),
                origins: vec![origin],
                attributes: candidate.attributes.clone(),
            });
        }
    }
    merged
}

fn validate_query(query: &str, max_query_bytes: NonZeroUsize) -> Result<(), MemoryRecallError> {
    if query.trim().is_empty() {
        return Err(MemoryRecallError::invalid_input(
            "memory recall query must not be blank",
        ));
    }
    if query
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(MemoryRecallError::invalid_input(
            "memory recall query contains a disallowed control character",
        ));
    }
    if query.len() > max_query_bytes.get() {
        return Err(MemoryRecallError::invalid_input(format!(
            "memory recall query is {} bytes, exceeding the {} byte limit",
            query.len(),
            max_query_bytes
        )));
    }
    Ok(())
}

fn validate_source_id_length(
    source_id: &RecallSourceId,
    max_source_id_bytes: NonZeroUsize,
) -> Result<(), CoordinatedMemoryRecallConfigError> {
    if source_id.as_str().len() > max_source_id_bytes.get() {
        return Err(CoordinatedMemoryRecallConfigError::SourceIdTooLong {
            actual_bytes: source_id.as_str().len(),
            max_bytes: max_source_id_bytes.get(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::oneshot;

    use super::*;
    use crate::RecallFailureKind;

    #[derive(Clone)]
    struct ImmediateSource {
        id: RecallSourceId,
        result: Result<RecallSourceResponse, RecallSourceError>,
    }

    impl RecallSource for ImmediateSource {
        fn id(&self) -> &RecallSourceId {
            &self.id
        }

        fn recall(
            &self,
            _request: RecallSourceRequest,
            _cancellation: CancellationToken,
        ) -> super::super::RecallSourceFuture<'_, RecallSourceResponse> {
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }

    struct ControlledSource {
        id: RecallSourceId,
        started: Mutex<Option<oneshot::Sender<()>>>,
        completion: Mutex<Option<oneshot::Receiver<RecallSourceResponse>>>,
    }

    impl RecallSource for ControlledSource {
        fn id(&self) -> &RecallSourceId {
            &self.id
        }

        fn recall(
            &self,
            _request: RecallSourceRequest,
            _cancellation: CancellationToken,
        ) -> super::super::RecallSourceFuture<'_, RecallSourceResponse> {
            if let Some(started) = self.started.lock().expect("lock started signal").take() {
                let _ = started.send(());
            }
            let completion = self
                .completion
                .lock()
                .expect("lock completion")
                .take()
                .expect("controlled source is called once");
            Box::pin(async move {
                completion.await.map_err(|_| RecallSourceError::Internal {
                    message: "controlled response channel closed".to_owned(),
                })
            })
        }
    }

    struct PendingSource {
        id: RecallSourceId,
    }

    impl RecallSource for PendingSource {
        fn id(&self) -> &RecallSourceId {
            &self.id
        }

        fn recall(
            &self,
            _request: RecallSourceRequest,
            _cancellation: CancellationToken,
        ) -> super::super::RecallSourceFuture<'_, RecallSourceResponse> {
            Box::pin(std::future::pending())
        }
    }

    fn id(value: &str) -> RecallSourceId {
        RecallSourceId::new(value).expect("valid source id")
    }

    fn item(content: &str) -> RecallSourceItem {
        RecallSourceItem {
            content: content.to_owned(),
            attributes: BTreeMap::new(),
            reference: Some(format!("ref-{content}")),
        }
    }

    fn response(contents: &[&str]) -> RecallSourceResponse {
        RecallSourceResponse {
            items: contents.iter().map(|content| item(content)).collect(),
            truncated: false,
        }
    }

    fn immediate(
        source_id: &str,
        result: Result<RecallSourceResponse, RecallSourceError>,
    ) -> Arc<dyn RecallSource> {
        Arc::new(ImmediateSource {
            id: id(source_id),
            result,
        })
    }

    fn config(default_sources: &[&str]) -> CoordinatedMemoryRecallConfig {
        CoordinatedMemoryRecallConfig {
            default_sources: default_sources.iter().map(|value| id(value)).collect(),
            source_timeout: Duration::from_secs(1),
            max_sources: NonZeroUsize::new(3).expect("non-zero"),
            max_query_bytes: NonZeroUsize::new(128).expect("non-zero"),
            max_source_id_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_item_bytes: NonZeroUsize::new(256).expect("non-zero"),
        }
    }

    fn request(limit: usize, sources: Option<Vec<&str>>) -> MemoryRecallRequest {
        MemoryRecallRequest {
            query: "preferred editor".to_owned(),
            limit: NonZeroUsize::new(limit).expect("non-zero"),
            sources: sources.map(|values| values.into_iter().map(id).collect()),
        }
    }

    #[test]
    fn coordinator_rejects_invalid_source_and_default_configuration() {
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![], config(&["notes"])),
            Err(CoordinatedMemoryRecallConfigError::NoSources)
        ));

        let notes = immediate("notes", Ok(response(&[])));
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes.clone()], config(&[])),
            Err(CoordinatedMemoryRecallConfigError::NoDefaultSources)
        ));
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes.clone(), notes.clone()], config(&["notes"])),
            Err(CoordinatedMemoryRecallConfigError::DuplicateSource { .. })
        ));
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes.clone()], config(&["unknown"])),
            Err(CoordinatedMemoryRecallConfigError::UnknownDefaultSource { .. })
        ));
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes.clone()], config(&["notes", "notes"])),
            Err(CoordinatedMemoryRecallConfigError::DuplicateDefaultSource { .. })
        ));

        let mut invalid = config(&["notes", "history"]);
        invalid.max_sources = NonZeroUsize::new(1).expect("non-zero");
        assert!(matches!(
            CoordinatedMemoryRecall::new(
                vec![notes.clone(), immediate("history", Ok(response(&[])))],
                invalid
            ),
            Err(CoordinatedMemoryRecallConfigError::TooManyDefaultSources { .. })
        ));

        let mut invalid = config(&["notes"]);
        invalid.source_timeout = Duration::ZERO;
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes.clone()], invalid),
            Err(CoordinatedMemoryRecallConfigError::ZeroSourceTimeout)
        ));

        let mut invalid = config(&["notes"]);
        invalid.max_source_id_bytes = NonZeroUsize::new(4).expect("non-zero");
        assert!(matches!(
            CoordinatedMemoryRecall::new(vec![notes], invalid),
            Err(CoordinatedMemoryRecallConfigError::SourceIdTooLong { .. })
        ));
    }

    #[tokio::test]
    async fn coordinator_uses_defaults_and_constructor_order_for_explicit_sources() {
        let recall = CoordinatedMemoryRecall::new(
            vec![
                immediate("history", Ok(response(&["history-result"]))),
                immediate("notes", Ok(response(&["notes-result"]))),
                immediate("remote", Ok(response(&["remote-result"]))),
            ],
            config(&["remote", "history"]),
        )
        .expect("valid coordinator");

        let defaults = recall
            .recall(request(3, None), CancellationToken::new())
            .await
            .expect("default recall");
        assert_eq!(
            defaults
                .items
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>(),
            vec!["history-result", "remote-result"]
        );

        let explicit = recall
            .recall(
                request(3, Some(vec!["notes", "history"])),
                CancellationToken::new(),
            )
            .await
            .expect("explicit recall");
        assert_eq!(
            explicit
                .items
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>(),
            vec!["history-result", "notes-result"]
        );
    }

    #[tokio::test]
    async fn coordinator_rejects_invalid_source_selection_and_query() {
        let mut coordinator_config = config(&["history"]);
        coordinator_config.max_sources = NonZeroUsize::new(2).expect("non-zero");
        let recall = CoordinatedMemoryRecall::new(
            vec![
                immediate("history", Ok(response(&[]))),
                immediate("notes", Ok(response(&[]))),
                immediate("remote", Ok(response(&[]))),
            ],
            coordinator_config,
        )
        .expect("valid coordinator");

        for invalid in [
            request(1, Some(vec![])),
            request(1, Some(vec!["history", "history"])),
            request(1, Some(vec!["unknown"])),
            request(1, Some(vec!["history", "notes", "remote"])),
        ] {
            assert!(matches!(
                recall.recall(invalid, CancellationToken::new()).await,
                Err(MemoryRecallError::InvalidInput { .. })
            ));
        }

        for query in [" ", "bad\0query"] {
            let mut invalid = request(1, None);
            invalid.query = query.to_owned();
            assert!(matches!(
                recall.recall(invalid, CancellationToken::new()).await,
                Err(MemoryRecallError::InvalidInput { .. })
            ));
        }
        let mut invalid = request(1, None);
        invalid.query = "x".repeat(129);
        assert!(matches!(
            recall.recall(invalid, CancellationToken::new()).await,
            Err(MemoryRecallError::InvalidInput { .. })
        ));
    }

    #[tokio::test]
    async fn coordinator_round_robins_exactly_deduplicates_origins_and_truncates() {
        let attributes = BTreeMap::from([(
            "kind".to_owned(),
            MemoryPropertyValue::String("preference".to_owned()),
        )]);
        let different_attributes = BTreeMap::from([(
            "kind".to_owned(),
            MemoryPropertyValue::String("project".to_owned()),
        )]);
        let duplicate_a = RecallSourceItem {
            content: "same content".to_owned(),
            attributes: attributes.clone(),
            reference: Some("history-1".to_owned()),
        };
        let duplicate_b = RecallSourceItem {
            content: "same content".to_owned(),
            attributes,
            reference: Some("notes-1".to_owned()),
        };
        let different = RecallSourceItem {
            content: "same content".to_owned(),
            attributes: different_attributes,
            reference: Some("notes-2".to_owned()),
        };
        let recall = CoordinatedMemoryRecall::new(
            vec![
                immediate(
                    "history",
                    Ok(RecallSourceResponse {
                        items: vec![duplicate_a, item("history-second"), item("history-third")],
                        truncated: false,
                    }),
                ),
                immediate(
                    "notes",
                    Ok(RecallSourceResponse {
                        items: vec![duplicate_b, different],
                        truncated: false,
                    }),
                ),
            ],
            config(&["history", "notes"]),
        )
        .expect("valid coordinator");

        let result = recall
            .recall(request(3, None), CancellationToken::new())
            .await
            .expect("recall succeeds");
        assert_eq!(result.items.len(), 3);
        assert!(result.truncated);
        assert_eq!(result.items[0].content, "same content");
        assert_eq!(
            result.items[0]
                .origins
                .iter()
                .map(|origin| origin.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["history", "notes"]
        );
        assert_eq!(result.items[1].content, "history-second");
        assert_eq!(result.items[2].content, "same content");
        assert_ne!(result.items[0].attributes, result.items[2].attributes);
    }

    #[tokio::test]
    async fn concurrent_completion_order_does_not_change_result_order() {
        let (started_history_tx, started_history_rx) = oneshot::channel();
        let (complete_history_tx, complete_history_rx) = oneshot::channel();
        let (started_notes_tx, started_notes_rx) = oneshot::channel();
        let (complete_notes_tx, complete_notes_rx) = oneshot::channel();
        let recall = Arc::new(
            CoordinatedMemoryRecall::new(
                vec![
                    Arc::new(ControlledSource {
                        id: id("history"),
                        started: Mutex::new(Some(started_history_tx)),
                        completion: Mutex::new(Some(complete_history_rx)),
                    }),
                    Arc::new(ControlledSource {
                        id: id("notes"),
                        started: Mutex::new(Some(started_notes_tx)),
                        completion: Mutex::new(Some(complete_notes_rx)),
                    }),
                ],
                config(&["history", "notes"]),
            )
            .expect("valid coordinator"),
        );

        let running = {
            let recall = recall.clone();
            tokio::spawn(async move {
                recall
                    .recall(request(2, None), CancellationToken::new())
                    .await
            })
        };
        started_history_rx.await.expect("history started");
        started_notes_rx.await.expect("notes started");
        complete_notes_tx
            .send(response(&["notes-result"]))
            .expect("complete notes first");
        complete_history_tx
            .send(response(&["history-result"]))
            .expect("complete history second");

        let result = running
            .await
            .expect("join recall")
            .expect("recall succeeds");
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>(),
            vec!["history-result", "notes-result"]
        );
    }

    #[tokio::test]
    async fn source_failures_are_partial_until_every_source_fails() {
        let unavailable = RecallSourceError::Unavailable {
            message: "temporarily unavailable".to_owned(),
        };
        let partial = CoordinatedMemoryRecall::new(
            vec![
                immediate("history", Ok(response(&[]))),
                immediate("notes", Err(unavailable.clone())),
            ],
            config(&["history", "notes"]),
        )
        .expect("valid coordinator")
        .recall(request(2, None), CancellationToken::new())
        .await
        .expect("one successful empty source is still success");
        assert!(partial.items.is_empty());
        assert_eq!(partial.failures.len(), 1);
        assert_eq!(partial.failures[0].kind, RecallFailureKind::Unavailable);

        let all_failed = CoordinatedMemoryRecall::new(
            vec![
                immediate("history", Err(unavailable)),
                immediate(
                    "notes",
                    Err(RecallSourceError::Io {
                        message: "read failed".to_owned(),
                    }),
                ),
            ],
            config(&["history", "notes"]),
        )
        .expect("valid coordinator")
        .recall(request(2, None), CancellationToken::new())
        .await;
        let Err(MemoryRecallError::AllSourcesFailed { failures }) = all_failed else {
            panic!("all failed sources must return structured error");
        };
        assert_eq!(
            failures
                .iter()
                .map(|failure| failure.kind)
                .collect::<Vec<_>>(),
            vec![RecallFailureKind::Unavailable, RecallFailureKind::Io]
        );
    }

    #[tokio::test]
    async fn source_contract_violations_are_isolated_as_invalid_data() {
        let mut coordinator_config = config(&["oversized", "good"]);
        coordinator_config.max_item_bytes = NonZeroUsize::new(12).expect("non-zero");
        let result = CoordinatedMemoryRecall::new(
            vec![
                immediate("oversized", Ok(response(&["content is much too long"]))),
                immediate("good", Ok(response(&["ok"]))),
            ],
            coordinator_config,
        )
        .expect("valid coordinator")
        .recall(request(2, None), CancellationToken::new())
        .await
        .expect("valid source remains usable");
        assert_eq!(result.items[0].content, "ok");
        assert_eq!(result.failures[0].kind, RecallFailureKind::InvalidData);

        let too_many = CoordinatedMemoryRecall::new(
            vec![immediate("notes", Ok(response(&["one", "two"])))],
            config(&["notes"]),
        )
        .expect("valid coordinator")
        .recall(request(1, None), CancellationToken::new())
        .await;
        assert!(matches!(
            too_many,
            Err(MemoryRecallError::AllSourcesFailed { failures })
                if failures[0].kind == RecallFailureKind::InvalidData
        ));
    }

    #[tokio::test]
    async fn source_timeout_is_a_partial_failure_without_sleep_ordering() {
        let mut coordinator_config = config(&["pending", "good"]);
        coordinator_config.source_timeout = Duration::from_millis(1);
        let result = CoordinatedMemoryRecall::new(
            vec![
                Arc::new(PendingSource { id: id("pending") }),
                immediate("good", Ok(response(&["usable"]))),
            ],
            coordinator_config,
        )
        .expect("valid coordinator")
        .recall(request(2, None), CancellationToken::new())
        .await
        .expect("good source survives timeout");
        assert_eq!(result.items[0].content, "usable");
        assert_eq!(result.failures[0].kind, RecallFailureKind::Timeout);
    }

    #[tokio::test]
    async fn global_cancellation_wins_over_partial_results() {
        let (started_tx, started_rx) = oneshot::channel();
        let (_complete_tx, complete_rx) = oneshot::channel();
        let recall = Arc::new(
            CoordinatedMemoryRecall::new(
                vec![
                    Arc::new(ControlledSource {
                        id: id("pending"),
                        started: Mutex::new(Some(started_tx)),
                        completion: Mutex::new(Some(complete_rx)),
                    }),
                    immediate("good", Ok(response(&["would-be-usable"]))),
                ],
                config(&["pending", "good"]),
            )
            .expect("valid coordinator"),
        );
        let cancellation = CancellationToken::new();
        let running = {
            let recall = recall.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move { recall.recall(request(2, None), cancellation).await })
        };
        started_rx.await.expect("pending source started");
        cancellation.cancel();
        assert_eq!(
            running.await.expect("join recall"),
            Err(MemoryRecallError::Cancelled)
        );

        let already_cancelled = CancellationToken::new();
        already_cancelled.cancel();
        let mut otherwise_invalid = request(2, None);
        otherwise_invalid.query = " ".to_owned();
        assert_eq!(
            recall.recall(otherwise_invalid, already_cancelled).await,
            Err(MemoryRecallError::Cancelled)
        );
    }

    #[tokio::test]
    async fn source_truncation_propagates_without_unified_limit_truncation() {
        let recall = CoordinatedMemoryRecall::new(
            vec![immediate(
                "notes",
                Ok(RecallSourceResponse {
                    items: vec![item("one")],
                    truncated: true,
                }),
            )],
            config(&["notes"]),
        )
        .expect("valid coordinator");
        let result = recall
            .recall(request(3, None), CancellationToken::new())
            .await
            .expect("recall succeeds");
        assert_eq!(result.items.len(), 1);
        assert!(result.truncated);
    }
}
