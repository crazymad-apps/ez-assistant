//! `recall_memory` 标准工具壳。
//!
//! 工具定义固定且不枚举 Source；Source ID 与用途由 System Prompt 或 Skill 告知模型。
//! resolve 只校验模型输入；检索委托给 [`MemoryRecall`]，稳定引用续读委托给可选的
//! [`RecallReferenceReader`]。

use std::{collections::BTreeSet, num::NonZeroUsize, sync::Arc};

use agent_memory::{
    MemoryRecall, MemoryRecallError, MemoryRecallRequest, MemoryRecallResponse,
    RecallReadDirection, RecallReferenceReadRequest, RecallReferenceReader, RecallScope,
    RecallSourceId,
};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};

/// `recall_memory` 的模型可见最大返回数量。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecallMemoryToolConfig {
    maximum_limit: NonZeroUsize,
}

impl RecallMemoryToolConfig {
    /// 创建只有一个非零结果上限的工具配置。
    pub fn new(maximum_limit: NonZeroUsize) -> Self {
        Self { maximum_limit }
    }

    /// 单次工具调用允许请求的最大结果数。
    pub fn maximum_limit(&self) -> NonZeroUsize {
        self.maximum_limit
    }
}

/// `recall_memory` 的判别联合模型输入；`limit` 始终必传。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecallMemoryInput {
    /// 检索召回候选。
    Search {
        /// 需要从大量历史或外部信息中检索的非空查询。
        query: String,
        /// 检索范围；省略时明确默认为当前 Session。
        #[serde(default)]
        scope: RecallScope,
        /// 明确要求返回的最大结果数。
        limit: NonZeroUsize,
        /// 可选 Source ID；省略时由统一能力使用显式默认集合。
        sources: Option<Vec<RecallSourceId>>,
    },
    /// 围绕搜索结果中的稳定引用继续读取有限正文。
    Read {
        /// 搜索结果返回的不透明引用。
        reference: String,
        /// 续读方向；省略时明确默认为命中附近。
        #[serde(default)]
        direction: RecallReadDirection,
        /// 明确要求返回的最大消息数。
        limit: NonZeroUsize,
    },
}

/// 已校验并按底层能力边界拆分的 `recall_memory` 输入。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ResolvedRecallMemoryInput {
    /// 多来源检索请求。
    Search(MemoryRecallRequest),
    /// 稳定引用续读请求。
    Read(RecallReferenceReadRequest),
}

/// `recall_memory`：通过统一能力按需查询一个或多个 Source。
pub struct RecallMemoryTool {
    recall: Arc<dyn MemoryRecall>,
    reference_reader: Option<Arc<dyn RecallReferenceReader>>,
    config: RecallMemoryToolConfig,
}

impl RecallMemoryTool {
    /// 用统一召回能力和模型可见结果上限装配工具壳。
    pub fn new(recall: Arc<dyn MemoryRecall>, config: RecallMemoryToolConfig) -> Self {
        Self {
            recall,
            reference_reader: None,
            config,
        }
    }

    /// 为具有稳定引用语义的数据源附加有限续读能力。
    #[must_use]
    pub fn with_reference_reader(mut self, reader: Arc<dyn RecallReferenceReader>) -> Self {
        self.reference_reader = Some(reader);
        self
    }
}

impl Tool for RecallMemoryTool {
    type Input = RecallMemoryInput;
    type ResolvedInput = ResolvedRecallMemoryInput;
    type Output = MemoryRecallResponse;

    fn name(&self) -> ToolName {
        ToolName::new("recall_memory").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Search configured memory sources or read bounded context around a returned reference. \
             Use action=search with query, scope=session|workspace|global, limit, and optional \
             sources. Use action=read with reference, direction=around|before|after, and limit. \
             Every limit is required and must not exceed {}. Results are temporary tool context \
             and are not automatically pinned.",
            self.config.maximum_limit
        )
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let resolved = match input {
            RecallMemoryInput::Search {
                query,
                scope,
                limit,
                sources,
            } => {
                validate_limit(limit, self.config.maximum_limit)?;
                if query.trim().is_empty() {
                    return Err(ToolError::invalid_input("query must not be blank"));
                }
                if query.chars().any(|character| {
                    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
                }) {
                    return Err(ToolError::invalid_input(
                        "query contains a disallowed control character",
                    ));
                }
                if let Some(sources) = &sources {
                    if sources.is_empty() {
                        return Err(ToolError::invalid_input(
                            "sources must be omitted or contain at least one Source ID",
                        ));
                    }
                    let mut unique = BTreeSet::new();
                    for source_id in sources {
                        if !unique.insert(source_id) {
                            return Err(ToolError::invalid_input(format!(
                                "duplicate recall source id `{source_id}`"
                            )));
                        }
                    }
                }
                ResolvedRecallMemoryInput::Search(MemoryRecallRequest {
                    query,
                    scope,
                    limit,
                    sources,
                })
            }
            RecallMemoryInput::Read {
                reference,
                direction,
                limit,
            } => {
                validate_limit(limit, self.config.maximum_limit)?;
                if reference.trim().is_empty() {
                    return Err(ToolError::invalid_input("reference must not be blank"));
                }
                if reference.chars().any(char::is_whitespace) {
                    return Err(ToolError::invalid_input(
                        "reference must not contain whitespace",
                    ));
                }
                ResolvedRecallMemoryInput::Read(RecallReferenceReadRequest {
                    reference,
                    direction,
                    limit,
                })
            }
        };
        Ok(ToolResolution::general(resolved))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            match input {
                ResolvedRecallMemoryInput::Search(request) => self
                    .recall
                    .recall(request, context.cancellation)
                    .await
                    .map_err(map_recall_error),
                ResolvedRecallMemoryInput::Read(request) => {
                    let reader = self.reference_reader.as_ref().ok_or_else(|| {
                        stable_recall_error(
                            "recall source does not support bounded reads",
                            "recall_read_unsupported",
                        )
                    })?;
                    reader
                        .read_reference(request, context.cancellation)
                        .await
                        .map_err(map_recall_error)
                }
            }
        })
    }
}

fn map_recall_error(error: MemoryRecallError) -> ToolError {
    match error {
        MemoryRecallError::InvalidInput { message } => ToolError::invalid_input(message),
        MemoryRecallError::AllSourcesFailed { failures } => ToolError::execution_with_details(
            "all selected recall sources failed",
            json!({
                "type": "all_sources_failed",
                "failures": failures,
            }),
        ),
        MemoryRecallError::ScopeUnavailable => stable_recall_error(
            "requested recall scope is unavailable",
            "recall_scope_unavailable",
        ),
        MemoryRecallError::ReferenceInvalid => {
            stable_recall_error("recall reference is invalid", "recall_reference_invalid")
        }
        MemoryRecallError::ReferenceStale => {
            stable_recall_error("recall reference is stale", "recall_reference_stale")
        }
        MemoryRecallError::SourceUnavailable => {
            stable_recall_error("recall source is unavailable", "recall_source_unavailable")
        }
        // Core 在全局取消后不会把这条文本回喂模型；该分支只保持错误映射完备。
        MemoryRecallError::Cancelled => ToolError::execution("memory recall cancelled"),
    }
}

fn validate_limit(limit: NonZeroUsize, maximum: NonZeroUsize) -> Result<(), ToolError> {
    if limit > maximum {
        return Err(ToolError::invalid_input(format!(
            "limit must not exceed {maximum}"
        )));
    }
    Ok(())
}

fn stable_recall_error(message: &'static str, error_type: &'static str) -> ToolError {
    ToolError::execution_with_details(message, json!({ "type": error_type }))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use agent_memory::{
        MemoryPropertyValue, MemoryRecallFailure, MemoryRecallFuture, RecallFailureKind,
        RecallItem, RecallOrigin,
    };
    use agent_types::{ToolResultContent, ToolResultStatus};

    use super::*;
    use crate::{
        Dispatcher, ResolvedBatchItemRef, ToolRegistry,
        testutil::{block_on, tool_call},
    };

    struct ProbeRecall {
        result: Result<MemoryRecallResponse, MemoryRecallError>,
        requests: Mutex<Vec<MemoryRecallRequest>>,
        read_requests: Mutex<Vec<RecallReferenceReadRequest>>,
    }

    impl ProbeRecall {
        fn new(result: Result<MemoryRecallResponse, MemoryRecallError>) -> Self {
            Self {
                result,
                requests: Mutex::new(Vec::new()),
                read_requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<MemoryRecallRequest> {
            self.requests.lock().expect("lock requests").clone()
        }

        fn read_requests(&self) -> Vec<RecallReferenceReadRequest> {
            self.read_requests.lock().expect("lock requests").clone()
        }
    }

    impl RecallReferenceReader for ProbeRecall {
        fn read_reference(
            &self,
            request: RecallReferenceReadRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> agent_memory::RecallReferenceReadFuture<'_, MemoryRecallResponse> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(MemoryRecallError::Cancelled);
                }
                self.read_requests
                    .lock()
                    .expect("lock requests")
                    .push(request);
                self.result.clone()
            })
        }
    }

    impl MemoryRecall for ProbeRecall {
        fn recall(
            &self,
            request: MemoryRecallRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> MemoryRecallFuture<'_, MemoryRecallResponse> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(MemoryRecallError::Cancelled);
                }
                self.requests.lock().expect("lock requests").push(request);
                self.result.clone()
            })
        }
    }

    fn config() -> RecallMemoryToolConfig {
        RecallMemoryToolConfig::new(NonZeroUsize::new(10).expect("non-zero"))
    }

    fn success() -> MemoryRecallResponse {
        MemoryRecallResponse {
            items: vec![RecallItem {
                content: "User prefers dark mode".to_owned(),
                origins: vec![RecallOrigin {
                    source_id: RecallSourceId::new("notes").expect("valid source id"),
                    reference: Some("note-1".to_owned()),
                }],
                attributes: BTreeMap::from([(
                    "kind".to_owned(),
                    MemoryPropertyValue::String("preference".to_owned()),
                )]),
            }],
            failures: vec![MemoryRecallFailure {
                source_id: RecallSourceId::new("remote").expect("valid source id"),
                kind: RecallFailureKind::Unavailable,
                message: "temporarily unavailable".to_owned(),
            }],
            truncated: false,
            window: None,
        }
    }

    fn execute(
        recall: Arc<dyn MemoryRecall>,
        arguments: serde_json::Value,
        context: ToolContext,
    ) -> agent_types::ToolResult {
        let mut registry = ToolRegistry::new();
        registry
            .register(RecallMemoryTool::new(recall, config()))
            .expect("register recall");
        let call = tool_call("recall_memory", arguments);
        let mut batch = Dispatcher::resolve_batch(&registry.snapshot(), &[call]);
        match batch.get(0) {
            Some(ResolvedBatchItemRef::Invalid { result, .. }) => result.clone(),
            Some(ResolvedBatchItemRef::Valid(_)) => {
                block_on(Dispatcher::execute(&mut batch, 0, context).expect("valid batch index"))
            }
            None => panic!("single recall call"),
        }
    }

    fn execute_with_reader(
        recall: Arc<ProbeRecall>,
        arguments: serde_json::Value,
    ) -> agent_types::ToolResult {
        let mut registry = ToolRegistry::new();
        registry
            .register(RecallMemoryTool::new(recall.clone(), config()).with_reference_reader(recall))
            .expect("register recall");
        let call = tool_call("recall_memory", arguments);
        let mut batch = Dispatcher::resolve_batch(&registry.snapshot(), &[call]);
        block_on(Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("valid batch"))
    }

    #[test]
    fn recall_definition_requires_limit_has_no_default_and_does_not_enumerate_sources() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        let mut registry = ToolRegistry::new();
        registry
            .register(RecallMemoryTool::new(recall, config()))
            .expect("register recall");
        let snapshot = registry.snapshot();
        let definition = &snapshot.definitions()[0];
        assert_eq!(definition.name.as_str(), "recall_memory");
        assert!(definition.description.contains("must not exceed 10"));
        assert!(!definition.description.contains("notes"));
        let serialized = definition.input_schema.to_string();
        assert!(serialized.contains("search"));
        assert!(serialized.contains("read"));
        assert!(serialized.contains("limit"));
        assert!(serialized.contains("session"));
    }

    #[test]
    fn recall_resolve_rejects_missing_limit_bounds_blank_and_duplicate_sources_without_execution() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        for arguments in [
            json!({"action": "search", "query": "editor"}),
            json!({"action": "search", "query": "editor", "limit": 11}),
            json!({"action": "search", "query": " ", "limit": 2}),
            json!({"action": "search", "query": "editor", "limit": 2, "sources": []}),
            json!({"action": "search", "query": "editor", "limit": 2, "sources": ["notes", "notes"]}),
            json!({"action": "read", "reference": " ", "limit": 2}),
        ] {
            let result = execute(recall.clone(), arguments, ToolContext::default());
            assert_eq!(result.status, ToolResultStatus::Error);
        }
        assert!(recall.requests().is_empty());
    }

    #[test]
    fn partial_failures_remain_a_successful_json_tool_result() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        let result = execute(
            recall.clone(),
            json!({"action": "search", "query": "theme", "limit": 2, "sources": ["notes", "remote"]}),
            ToolContext::default(),
        );
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(
            result.content,
            ToolResultContent::json(json!({
                "items": [{
                    "content": "User prefers dark mode",
                    "origins": [{"source_id": "notes", "reference": "note-1"}],
                    "attributes": {"kind": "preference"}
                }],
                "failures": [{
                    "source_id": "remote",
                    "kind": "unavailable",
                    "message": "temporarily unavailable"
                }],
                "truncated": false,
                "window": null
            }))
        );
        assert_eq!(recall.requests().len(), 1);
    }

    #[test]
    fn read_routes_only_to_the_optional_reference_reader() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        let result = execute_with_reader(
            recall.clone(),
            json!({"action": "read", "reference": "ref-1", "direction": "before", "limit": 2}),
        );
        assert_eq!(result.status, ToolResultStatus::Success);
        assert!(recall.requests().is_empty());
        assert_eq!(recall.read_requests().len(), 1);
    }

    #[test]
    fn read_without_reference_capability_returns_a_stable_error() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        let result = execute(
            recall,
            json!({"action": "read", "reference": "ref-1", "limit": 2}),
            ToolContext::default(),
        );
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(
            result
                .content
                .as_single_json()
                .and_then(|value| value["error"]["details"]["type"].as_str()),
            Some("recall_read_unsupported")
        );
    }

    #[test]
    fn all_sources_failed_returns_structured_error_details_with_original_call_id() {
        let failures = vec![MemoryRecallFailure {
            source_id: RecallSourceId::new("notes").expect("valid source id"),
            kind: RecallFailureKind::Io,
            message: "read failed".to_owned(),
        }];
        let recall = Arc::new(ProbeRecall::new(Err(MemoryRecallError::AllSourcesFailed {
            failures: failures.clone(),
        })));
        let result = execute(
            recall,
            json!({"action": "search", "query": "theme", "limit": 2}),
            ToolContext::default(),
        );
        assert_eq!(result.call_id.as_str(), "call_1");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(
            result.content,
            ToolResultContent::json(json!({
                "error": {
                    "message": "all selected recall sources failed",
                    "details": {
                        "type": "all_sources_failed",
                        "failures": failures
                    }
                }
            }))
        );
    }

    #[test]
    fn cancelled_recall_maps_to_an_error_result_for_direct_dispatch() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let result = execute(
            recall,
            json!({"action": "search", "query": "theme", "limit": 2}),
            ToolContext::new(cancellation, Arc::new(|_| {})),
        );
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(
            result
                .content
                .as_single_text()
                .is_some_and(|message| message.contains("cancelled"))
        );
    }
}
