//! Tool Call 整批无副作用解析与 resolved invocation 一次性执行。

use std::sync::Mutex;

use agent_types::ToolCall;
use thiserror::Error;

use crate::{
    ToolContext, ToolJsonFuture,
    registry::ToolSetSnapshot,
    resolution::{ResolvedBatchItem, ResolvedToolBatch, ready_result},
    tool::{text_error_result, text_error_result_for_id},
};

/// 调用方传入无效 batch 位置时的 Dispatcher 契约错误。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DispatchError {
    /// 请求位置不存在于 resolved batch。
    #[error("resolved tool batch index {index} is out of bounds for length {len}")]
    IndexOutOfBounds {
        /// 调用方请求的位置。
        index: usize,
        /// batch 的实际长度。
        len: usize,
    },
}

/// resolved invocation 解析与执行派发器。
pub struct Dispatcher;

impl Dispatcher {
    /// 对完整 Tool Call 批次做无 I/O、无副作用解析。
    ///
    /// 结果严格保留原数量和顺序；单项失败只影响当前位置。
    pub fn resolve_batch(snapshot: &ToolSetSnapshot, calls: &[ToolCall]) -> ResolvedToolBatch {
        let items = calls
            .iter()
            .map(|call| match snapshot.tool(&call.name) {
                Some(tool) => match tool.resolve(call) {
                    Ok(resolved) => ResolvedBatchItem::Valid {
                        invocation: resolved.invocation,
                        executor: Mutex::new(Some(resolved.executor)),
                    },
                    Err(result) => ResolvedBatchItem::Invalid {
                        tool_name: call.name.clone(),
                        result,
                    },
                },
                None => ResolvedBatchItem::Invalid {
                    tool_name: call.name.clone(),
                    result: text_error_result(call, format!("unknown tool: `{}`", call.name)),
                },
            })
            .collect();
        ResolvedToolBatch { items }
    }

    /// 执行一个解析成功的位置，并消费该位置的一次性 executor。
    ///
    /// 执行 Invalid 位置或重复执行会得到绑定原 call ID 的内部契约错误。
    /// 仅位置越界直接返回 [`DispatchError`]，因为此时没有可绑定的协议 call ID。
    pub fn execute(
        batch: &mut ResolvedToolBatch,
        index: usize,
        context: ToolContext,
    ) -> Result<ToolJsonFuture<'static>, DispatchError> {
        let len = batch.items.len();
        let item = batch
            .items
            .get_mut(index)
            .ok_or(DispatchError::IndexOutOfBounds { index, len })?;
        match item {
            ResolvedBatchItem::Invalid { result, .. } => {
                Ok(ready_result(text_error_result_for_id(
                    result.call_id.clone(),
                    "dispatcher contract violation: cannot execute an invalid resolved item"
                        .to_owned(),
                )))
            }
            ResolvedBatchItem::Valid {
                invocation,
                executor,
            } => match executor
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                Some(executor) => Ok(executor.execute(context)),
                None => Ok(ready_result(text_error_result_for_id(
                    invocation.call_id().clone(),
                    format!(
                        "dispatcher contract violation: resolved tool `{}` was already executed",
                        invocation.tool_name()
                    ),
                ))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use agent_types::{ToolName, ToolResultContent, ToolResultStatus};
    use serde_json::json;

    use super::*;
    use crate::{
        GeneralAuthorizationFacts, ResolvedBatchItemRef, Tool, ToolError, ToolExecuteFuture,
        ToolRegistry, ToolResolution,
        testutil::{AddTool, FailTool, block_on, tool_call},
    };

    fn snapshot() -> ToolSetSnapshot {
        let mut registry = ToolRegistry::new();
        registry.register(AddTool).expect("register add tool");
        registry.register(FailTool).expect("register fail tool");
        registry.snapshot()
    }

    #[test]
    fn whole_batch_resolution_preserves_valid_and_invalid_order() {
        let calls = [
            tool_call("add", json!({"a": 40, "b": 2})),
            tool_call("missing", json!({})),
            tool_call("add", json!({"a": "wrong", "b": 2})),
        ];
        let batch = Dispatcher::resolve_batch(&snapshot(), &calls);
        assert_eq!(batch.len(), 3);
        assert!(matches!(batch.get(0), Some(ResolvedBatchItemRef::Valid(_))));
        assert!(matches!(
            batch.get(1),
            Some(ResolvedBatchItemRef::Invalid { result, .. })
                if result.call_id == calls[1].id
        ));
        assert!(matches!(
            batch.get(2),
            Some(ResolvedBatchItemRef::Invalid { result, .. })
                if result.call_id == calls[2].id
        ));
    }

    #[test]
    fn resolved_description_facts_fingerprint_and_execution_share_one_input() {
        let call = tool_call("add", json!({"a": 40, "b": 2}));
        let mut batch = Dispatcher::resolve_batch(&snapshot(), std::slice::from_ref(&call));
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("add resolves");
        };
        assert_eq!(invocation.call_id(), &call.id);
        assert_eq!(invocation.resolved_arguments(), &json!({"a": 40, "b": 2}));
        assert_eq!(
            invocation
                .facts::<GeneralAuthorizationFacts>()
                .expect("general facts")
                .tool_name,
            call.name
        );
        assert!(invocation.facts::<String>().is_none());
        assert_eq!(
            invocation.fingerprint().semantic_arguments(),
            &json!({"a": 40, "b": 2})
        );

        let result = block_on(
            Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("valid index"),
        );
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.content, ToolResultContent::json(json!({"sum": 42})));

        let repeated = block_on(
            Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("valid index"),
        );
        assert_eq!(repeated.call_id, call.id);
        assert_eq!(repeated.status, ToolResultStatus::Error);
    }

    #[test]
    fn execution_failure_stays_bound_to_the_original_call() {
        let call = tool_call("fail", json!({"a": 1, "b": 2}));
        let mut batch = Dispatcher::resolve_batch(&snapshot(), std::slice::from_ref(&call));
        let result = block_on(
            Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("valid index"),
        );
        assert_eq!(result.call_id, call.id);
        assert_eq!(result.status, ToolResultStatus::Error);
        let Some(message) = result.content.as_single_text() else {
            panic!("plain execution failure is text");
        };
        assert!(message.contains("boom"));
    }

    struct ResolveFlagTool {
        executions: Arc<AtomicUsize>,
    }

    impl Tool for ResolveFlagTool {
        type Input = serde_json::Value;
        type ResolvedInput = serde_json::Value;
        type Output = serde_json::Value;

        fn name(&self) -> ToolName {
            ToolName::new("resolve_flag").expect("valid name")
        }

        fn description(&self) -> String {
            "resolve flag".to_owned()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            if input == json!({"deny": true}) {
                return Err(ToolError::invalid_input("rejected in resolve"));
            }
            if input == json!({"wrong_phase": true}) {
                return Err(ToolError::execution("incorrect resolve error"));
            }
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    #[test]
    fn resolve_failure_never_creates_an_executor() {
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(ResolveFlagTool {
                executions: executions.clone(),
            })
            .expect("register tool");
        let snapshot = registry.snapshot();
        let call = tool_call("resolve_flag", json!({"deny": true}));
        let mut batch = Dispatcher::resolve_batch(&snapshot, &[call]);
        let result = block_on(
            Dispatcher::execute(&mut batch, 0, ToolContext::default()).expect("valid index"),
        );
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn resolve_execution_error_is_normalized_to_invalid_input() {
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(ResolveFlagTool {
                executions: executions.clone(),
            })
            .expect("register tool");
        let snapshot = registry.snapshot();
        let call = tool_call("resolve_flag", json!({"wrong_phase": true}));
        let batch = Dispatcher::resolve_batch(&snapshot, &[call]);
        let Some(ResolvedBatchItemRef::Invalid { result, .. }) = batch.get(0) else {
            panic!("resolve error creates invalid item");
        };
        let Some(message) = result.content.as_single_text() else {
            panic!("resolve error is model-visible text");
        };
        assert!(message.starts_with("invalid tool input:"));
        assert!(message.contains("incorrect resolve error"));
        assert_eq!(executions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn out_of_bounds_is_an_explicit_caller_error() {
        let mut batch = Dispatcher::resolve_batch(&snapshot(), &[]);
        let error = match Dispatcher::execute(&mut batch, 0, ToolContext::default()) {
            Ok(_) => panic!("out of bounds must fail"),
            Err(error) => error,
        };
        assert_eq!(error, DispatchError::IndexOutOfBounds { index: 0, len: 0 });
    }
}
