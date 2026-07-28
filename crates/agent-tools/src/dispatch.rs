//! 单次规范 Tool Call 派发。
//!
//! [`Dispatcher`] 一次只处理一个规范 Tool Call：按名称查表并交给擦除后的工具；
//! 未知名、校验失败、执行失败和异常结果 ID 都转为绑定原调用 ID 的模型可读
//! 错误 `ToolResult`。
//! Dispatcher 不负责模型请求与 Agent 继续循环。

use std::{future::Future, pin::Pin};

use agent_types::{ToolCall, ToolResult, ToolResultContent, ToolResultStatus};

use crate::{registry::ToolSetSnapshot, tool::ToolContext};

/// 规范 Tool Call 派发器。
pub struct Dispatcher;

impl Dispatcher {
    /// 派发一次 Tool Call；任何失败路径都表达为错误 `ToolResult`。
    pub fn dispatch<'a>(
        snapshot: &'a ToolSetSnapshot,
        call: &'a ToolCall,
        context: ToolContext,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        match snapshot.tool(&call.name) {
            Some(tool) => Box::pin(async move {
                let result = tool.execute_json(call, context).await;
                if result.call_id == call.id {
                    result
                } else {
                    ToolResult {
                        call_id: call.id.clone(),
                        status: ToolResultStatus::Error,
                        content: ToolResultContent::Text(format!(
                            "tool `{}` returned a result for a different call id",
                            call.name
                        )),
                    }
                }
            }),
            None => Box::pin(std::future::ready(ToolResult {
                call_id: call.id.clone(),
                status: ToolResultStatus::Error,
                content: ToolResultContent::Text(format!("unknown tool: `{}`", call.name)),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_types::{ToolCallId, ToolDefinition, ToolName};
    use serde_json::json;

    use super::*;
    use crate::{
        ErasedTool, ToolJsonFuture,
        registry::ToolRegistry,
        testutil::{AddTool, FailTool, block_on, tool_call},
    };

    fn snapshot() -> ToolSetSnapshot {
        let mut registry = ToolRegistry::new();
        registry.register(AddTool).expect("register add tool");
        registry.register(FailTool).expect("register fail tool");
        registry.snapshot()
    }

    #[test]
    fn unknown_tool_returns_error_result() {
        let snapshot = snapshot();
        let call = tool_call("missing", json!({}));
        let result = block_on(Dispatcher::dispatch(
            &snapshot,
            &call,
            ToolContext::default(),
        ));
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.call_id, call.id);
        let ToolResultContent::Text(message) = result.content else {
            panic!("error result must carry model-readable text");
        };
        assert!(message.contains("unknown tool: `missing`"));
    }

    #[test]
    fn dispatch_routes_call_to_matching_tool() {
        let snapshot = snapshot();
        let add = tool_call("add", json!({"a": 40, "b": 2}));
        let result = block_on(Dispatcher::dispatch(
            &snapshot,
            &add,
            ToolContext::default(),
        ));
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.content, ToolResultContent::Json(json!({"sum": 42})));

        let fail = tool_call("fail", json!({"a": 1, "b": 2}));
        let result = block_on(Dispatcher::dispatch(
            &snapshot,
            &fail,
            ToolContext::default(),
        ));
        assert_eq!(result.status, ToolResultStatus::Error);
    }

    struct WrongCallIdTool;

    impl ErasedTool for WrongCallIdTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: ToolName::new("wrong_id").expect("valid tool name"),
                description: "returns a mismatched call id".to_owned(),
                input_schema: json!({"type": "object"}),
            }
        }

        fn execute_json<'a>(
            &'a self,
            _call: &'a ToolCall,
            _context: ToolContext,
        ) -> ToolJsonFuture<'a> {
            Box::pin(std::future::ready(ToolResult {
                call_id: ToolCallId::new("different_call").expect("valid call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Json(json!({"unsafe": true})),
            }))
        }
    }

    #[test]
    fn mismatched_result_id_becomes_error_for_original_call() {
        let mut registry = ToolRegistry::new();
        registry
            .register_erased(Arc::new(WrongCallIdTool))
            .expect("register erased tool");
        let snapshot = registry.snapshot();
        let call = tool_call("wrong_id", json!({}));
        let result = block_on(Dispatcher::dispatch(
            &snapshot,
            &call,
            ToolContext::default(),
        ));
        assert_eq!(result.call_id, call.id);
        assert_eq!(result.status, ToolResultStatus::Error);
        let ToolResultContent::Text(message) = result.content else {
            panic!("contract violation must be model-readable text");
        };
        assert!(message.contains("different call id"));
    }
}
