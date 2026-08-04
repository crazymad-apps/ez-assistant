//! `recall_memory` 标准工具壳。
//!
//! 工具定义固定且不枚举 Source；Source ID 与用途由 System Prompt 或 Skill 告知模型。
//! resolve 只校验模型输入，实际 Source 选择与访问全部委托给 [`MemoryRecall`]。

use std::{collections::BTreeSet, num::NonZeroUsize, sync::Arc};

use agent_memory::{
    MemoryRecall, MemoryRecallError, MemoryRecallRequest, MemoryRecallResponse, RecallSourceId,
};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::Deserialize;
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

/// `recall_memory` 的固定模型输入；`limit` 必传且没有隐藏默认值。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecallMemoryInput {
    /// 需要从大量历史或外部信息中检索的非空查询。
    pub query: String,
    /// 明确要求返回的最大结果数。
    pub limit: NonZeroUsize,
    /// 可选 Source ID；省略时由统一能力使用显式默认集合。
    pub sources: Option<Vec<RecallSourceId>>,
}

/// `recall_memory`：通过统一能力按需查询一个或多个 Source。
pub struct RecallMemoryTool {
    recall: Arc<dyn MemoryRecall>,
    config: RecallMemoryToolConfig,
}

impl RecallMemoryTool {
    /// 用统一召回能力和模型可见结果上限装配工具壳。
    pub fn new(recall: Arc<dyn MemoryRecall>, config: RecallMemoryToolConfig) -> Self {
        Self { recall, config }
    }
}

impl Tool for RecallMemoryTool {
    type Input = RecallMemoryInput;
    type ResolvedInput = MemoryRecallRequest;
    type Output = MemoryRecallResponse;

    fn name(&self) -> ToolName {
        ToolName::new("recall_memory").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Recall relevant information from configured memory sources. limit is required, has \
             no default, and must not exceed {}. Omit sources to use the configured defaults, or \
             provide Source IDs described by the current system prompt or skills. Results are \
             temporary tool context and are not automatically pinned.",
            self.config.maximum_limit
        )
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.query.trim().is_empty() {
            return Err(ToolError::invalid_input("query must not be blank"));
        }
        if input
            .query
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(ToolError::invalid_input(
                "query contains a disallowed control character",
            ));
        }
        if input.limit > self.config.maximum_limit {
            return Err(ToolError::invalid_input(format!(
                "limit must not exceed {}",
                self.config.maximum_limit
            )));
        }
        if let Some(sources) = &input.sources {
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
        Ok(ToolResolution::general(MemoryRecallRequest {
            query: input.query,
            limit: input.limit,
            sources: input.sources,
        }))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.recall
                .recall(input, context.cancellation)
                .await
                .map_err(map_recall_error)
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
        // Core 在全局取消后不会把这条文本回喂模型；该分支只保持错误映射完备。
        MemoryRecallError::Cancelled => ToolError::execution("memory recall cancelled"),
    }
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
    }

    impl ProbeRecall {
        fn new(result: Result<MemoryRecallResponse, MemoryRecallError>) -> Self {
            Self {
                result,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<MemoryRecallRequest> {
            self.requests.lock().expect("lock requests").clone()
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
            Some(ResolvedBatchItemRef::Invalid(result)) => result.clone(),
            Some(ResolvedBatchItemRef::Valid(_)) => {
                block_on(Dispatcher::execute(&mut batch, 0, context).expect("valid batch index"))
            }
            None => panic!("single recall call"),
        }
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
        assert!(
            definition.input_schema["required"]
                .as_array()
                .expect("required array")
                .contains(&json!("limit"))
        );
        assert!(
            definition.input_schema["properties"]["limit"]
                .get("default")
                .is_none()
        );
    }

    #[test]
    fn recall_resolve_rejects_missing_limit_bounds_blank_and_duplicate_sources_without_execution() {
        let recall = Arc::new(ProbeRecall::new(Ok(success())));
        for arguments in [
            json!({"query": "editor"}),
            json!({"query": "editor", "limit": 11}),
            json!({"query": " ", "limit": 2}),
            json!({"query": "editor", "limit": 2, "sources": []}),
            json!({"query": "editor", "limit": 2, "sources": ["notes", "notes"]}),
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
            json!({"query": "theme", "limit": 2, "sources": ["notes", "remote"]}),
            ToolContext::default(),
        );
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(
            result.content,
            ToolResultContent::Json(json!({
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
                "truncated": false
            }))
        );
        assert_eq!(recall.requests().len(), 1);
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
            json!({"query": "theme", "limit": 2}),
            ToolContext::default(),
        );
        assert_eq!(result.call_id.as_str(), "call_1");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(
            result.content,
            ToolResultContent::Json(json!({
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
            json!({"query": "theme", "limit": 2}),
            ToolContext::new(cancellation, Arc::new(|_| {})),
        );
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(
            matches!(result.content, ToolResultContent::Text(message) if message.contains("cancelled"))
        );
    }
}
