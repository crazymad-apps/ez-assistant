//! 类型化 Tool 抽象与类型擦除。
//!
//! 工具作者实现 [`Tool`]：输入类型经 serde 反序列化即校验，JSON Schema 由
//! schemars 从 `Input` 类型派生，两者恒同步。注册表持有对象安全擦除后的
//! [`ErasedTool`]，派发路径固定为：
//!
//! ```text
//! serde 反序列化校验（失败即模型可读的错误 ToolResult）
//!     → 类型化 execute
//!     → 序列化输出
//!     → 规范 ToolResult
//! ```
//!
//! 取消不是 [`ToolError`]：长任务实现必须观察 [`ToolContext::cancellation`]，
//! 取消语义由引擎在外围处理。

use std::{future::Future, pin::Pin, sync::Arc};

use agent_types::{
    ToolCall, ToolDefinition, ToolName, ToolResult, ToolResultContent, ToolResultStatus,
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// 类型化工具执行的 Future。
pub type ToolExecuteFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, ToolError>> + Send + 'a>>;

/// 类型擦除后按规范 Tool Call 执行的 Future。
pub type ToolJsonFuture<'a> = Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;

/// 工具流式输出通道。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputChannel {
    /// 标准输出或普通文本增量。
    Stdout,
    /// 标准错误。
    Stderr,
}

/// 工具执行过程中的流式输出片段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutputChunk {
    /// 输出通道。
    pub channel: ToolOutputChannel,
    /// 面向观察者的增量文本。
    pub delta: String,
}

/// 工具流式输出回调；引擎桥接为 `AgentEvent::ToolOutput`。
pub type ToolOutputSink = Arc<dyn Fn(ToolOutputChunk) + Send + Sync>;

/// 单次工具调用的控制面。
///
/// 与工具语义输入（`Input`）分离；实现方必须在长任务中观察 `cancellation`，
/// 可以通过 `output_sink` 发出面向观察者的流式输出。
#[derive(Clone)]
pub struct ToolContext {
    /// 取消信号；取消不是 [`ToolError`]。
    pub cancellation: CancellationToken,
    /// 流式输出回调。
    pub output_sink: ToolOutputSink,
}

impl ToolContext {
    /// 创建携带取消信号与输出回调的工具上下文。
    pub fn new(cancellation: CancellationToken, output_sink: ToolOutputSink) -> Self {
        Self {
            cancellation,
            output_sink,
        }
    }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            output_sink: Arc::new(|_| {}),
        }
    }
}

/// 工具失败分类；两种失败都转为错误 `ToolResult` 回喂模型。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ToolError {
    /// 输入校验或参数约束失败。
    #[error("invalid tool input: {message}")]
    InvalidInput {
        /// 模型可读的失败原因。
        message: String,
    },
    /// 执行过程失败。
    #[error("tool execution failed: {message}")]
    Execution {
        /// 模型可读的失败原因。
        message: String,
    },
}

impl ToolError {
    /// 构造输入校验失败。
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    /// 构造执行失败。
    pub fn execution(message: impl Into<String>) -> Self {
        Self::Execution {
            message: message.into(),
        }
    }
}

/// 类型化 Agent 工具。
///
/// `definition()` 的默认实现用 schemars 从 `Input` 派生 `input_schema`。
/// 经 [`crate::ToolRegistry::register`] 注册时，擦除层固定以 `Input` 派生
/// schema（覆盖本方法返回的 `input_schema`），schema 与反序列化校验的同步
/// 由构造保证；经 `register_erased` 注册的自定义 [`ErasedTool`] 自行负责
/// schema 与实际反序列化规则的一致性。完整 JSON Schema 约束校验
/// （pattern/minimum 等）不在本层承担。
pub trait Tool: Send + Sync {
    /// 工具输入；serde 反序列化即校验（`deny_unknown_fields` 等由类型作者自决）。
    type Input: DeserializeOwned + JsonSchema;
    /// 工具输出；序列化为 JSON 后进入 [`ToolResultContent::Json`]。
    type Output: Serialize;

    /// 模型可见的工具名称。
    fn name(&self) -> ToolName;

    /// 面向模型的工具用途说明。
    fn description(&self) -> &str;

    /// 模型可见的工具定义；默认实现保证 schema 与 `Input` 类型同步。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name(),
            description: self.description().to_owned(),
            input_schema: input_schema_value::<Self::Input>(),
        }
    }

    /// 执行一次工具调用。
    fn execute<'a>(
        &'a self,
        input: Self::Input,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output>;
}

/// 对象安全擦除后的工具；注册表与派发器统一持有。
pub trait ErasedTool: Send + Sync {
    /// 模型可见的工具定义。
    fn definition(&self) -> ToolDefinition;

    /// 按规范 Tool Call 执行：serde 反序列化校验 → 类型化 execute → 序列化输出。
    fn execute_json<'a>(&'a self, call: &'a ToolCall, context: ToolContext) -> ToolJsonFuture<'a>;
}

/// 把类型化 [`Tool`] 擦除为 [`ErasedTool`]。
pub(crate) struct TypedToolErasure<T>(pub T);

impl<T: Tool> ErasedTool for TypedToolErasure<T> {
    fn definition(&self) -> ToolDefinition {
        let mut definition = self.0.definition();
        // schema 与 Input 类型的同步由擦除层强制，不依赖实现方自觉。
        definition.input_schema = input_schema_value::<T::Input>();
        definition
    }

    fn execute_json<'a>(&'a self, call: &'a ToolCall, context: ToolContext) -> ToolJsonFuture<'a> {
        let input = match serde_json::from_value::<T::Input>(call.arguments.clone()) {
            Ok(input) => input,
            Err(error) => {
                let result = error_result(
                    call,
                    format!("invalid arguments for tool `{}`: {error}", call.name),
                );
                return Box::pin(std::future::ready(result));
            }
        };
        let future = self.0.execute(input, context);
        Box::pin(async move {
            match future.await {
                Ok(output) => match serde_json::to_value(output) {
                    Ok(value) => ToolResult {
                        call_id: call.id.clone(),
                        status: ToolResultStatus::Success,
                        content: ToolResultContent::Json(value),
                    },
                    Err(error) => error_result(
                        call,
                        format!(
                            "failed to serialize output of tool `{}`: {error}",
                            call.name
                        ),
                    ),
                },
                Err(error) => error_result(call, error.to_string()),
            }
        })
    }
}

/// 从 `Input` 类型派生 JSON Schema。
///
/// schemars 的 `Schema` 只承载 JSON 兼容数据，序列化为 `Value` 不会失败。
fn input_schema_value<Input: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(Input))
        .expect("schemars schema serialization is infallible")
}

/// 统一构造错误 `ToolResult`；内容为模型可读的文本。
fn error_result(call: &ToolCall, message: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        status: ToolResultStatus::Error,
        content: ToolResultContent::Text(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use serde_json::json;

    use super::*;
    use crate::testutil::{AddTool, FailTool, block_on, tool_call};

    /// 记录是否进入 execute 的工具，用于验证校验失败不触达实现。
    struct FlagTool {
        entered: Arc<AtomicBool>,
    }

    impl Tool for FlagTool {
        type Input = crate::testutil::AddInput;
        type Output = crate::testutil::AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("flag").expect("valid tool name")
        }

        fn description(&self) -> &str {
            "Record whether execute is entered"
        }

        fn execute<'a>(
            &'a self,
            input: Self::Input,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(async move {
                self.entered.store(true, Ordering::SeqCst);
                Ok(crate::testutil::AddOutput { sum: input.a })
            })
        }
    }

    fn erased<T: Tool>(tool: T) -> impl ErasedTool {
        TypedToolErasure(tool)
    }

    #[test]
    fn definition_derives_schema_from_input_type() {
        let definition = AddTool.definition();
        assert_eq!(definition.name.as_str(), "add");
        assert_eq!(definition.description, "Add two integers");
        let properties = definition
            .input_schema
            .get("properties")
            .expect("schema has properties");
        assert!(properties.get("a").is_some());
        assert!(properties.get("b").is_some());
    }

    #[test]
    fn valid_arguments_execute_and_serialize_output() {
        let tool = erased(AddTool);
        let call = tool_call("add", json!({"a": 1, "b": 2}));
        let result = block_on(tool.execute_json(&call, ToolContext::default()));
        assert_eq!(result.call_id, call.id);
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(result.content, ToolResultContent::Json(json!({"sum": 3})));
    }

    #[test]
    fn invalid_arguments_return_error_result_without_entering_execute() {
        let entered = Arc::new(AtomicBool::new(false));
        let tool = erased(FlagTool {
            entered: entered.clone(),
        });

        // 类型错误：serde 反序列化失败，不进入实现。
        let wrong_type = tool_call("flag", json!({"a": "not-a-number", "b": 2}));
        let result = block_on(tool.execute_json(&wrong_type, ToolContext::default()));
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.call_id, wrong_type.id);
        let ToolResultContent::Text(message) = result.content else {
            panic!("error result must carry model-readable text");
        };
        assert!(message.contains("invalid arguments for tool `flag`"));

        // 未知字段：类型作者声明 deny_unknown_fields 后同样在校验阶段失败。
        let unknown_field = tool_call("flag", json!({"a": 1, "b": 2, "c": 3}));
        let result = block_on(tool.execute_json(&unknown_field, ToolContext::default()));
        assert_eq!(result.status, ToolResultStatus::Error);

        assert!(!entered.load(Ordering::SeqCst));
    }

    #[test]
    fn execution_error_maps_to_error_result() {
        let tool = erased(FailTool);
        let call = tool_call("fail", json!({"a": 1, "b": 2}));
        let result = block_on(tool.execute_json(&call, ToolContext::default()));
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.call_id, call.id);
        let ToolResultContent::Text(message) = result.content else {
            panic!("error result must carry model-readable text");
        };
        assert!(message.contains("boom"));
    }

    #[test]
    fn output_channel_round_trips_serde() {
        for channel in [ToolOutputChannel::Stdout, ToolOutputChannel::Stderr] {
            let json = serde_json::to_string(&channel).expect("serialize channel");
            assert_eq!(
                serde_json::from_str::<ToolOutputChannel>(&json).expect("deserialize channel"),
                channel
            );
        }
        assert_eq!(
            serde_json::to_string(&ToolOutputChannel::Stdout).expect("serialize channel"),
            "\"stdout\""
        );
    }

    /// 覆盖 definition() 并提供与 Input 反序列化规则不一致 schema 的工具。
    struct BogusSchemaTool;

    impl Tool for BogusSchemaTool {
        type Input = crate::testutil::AddInput;
        type Output = crate::testutil::AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("bogus").expect("valid tool name")
        }

        fn description(&self) -> &str {
            "Override definition with a bogus schema"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name(),
                description: self.description().to_owned(),
                input_schema: json!({"type": "object", "properties": {"unrelated": {}}}),
            }
        }

        fn execute<'a>(
            &'a self,
            input: Self::Input,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(async move { Ok(crate::testutil::AddOutput { sum: input.a }) })
        }
    }

    #[test]
    fn erasure_pins_schema_to_input_type_when_definition_is_overridden() {
        let definition = erased(BogusSchemaTool).definition();
        assert_eq!(definition.name.as_str(), "bogus");
        let properties = definition
            .input_schema
            .get("properties")
            .expect("schema has properties");
        assert!(properties.get("a").is_some());
        assert!(properties.get("b").is_some());
        assert!(properties.get("unrelated").is_none());
    }
}
