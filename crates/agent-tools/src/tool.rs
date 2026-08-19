//! 类型化 Tool 生命周期与 crate 内部类型擦除。
//!
//! 工具作者只需实现 [`Tool`]。Registry 一次冻结模型可见定义，
//! Dispatcher 只使用下列唯一路径：
//!
//! ```text
//! 反序列化 Input
//! → 无副作用 resolve
//! → 冻结公开 facts/fingerprint 与一次性 executor
//! → 执行 ResolvedInput
//! → 序列化 ToolResult
//! ```
//!
//! 取消不是 [`ToolError`]。长时间运行的实现必须观察
//! [`ToolContext::cancellation`]，并在返回前完成资源清理。

use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc};

use agent_types::{
    ToolCall, ToolCallId, ToolDefinition, ToolName, ToolResult, ToolResultContent, ToolResultStatus,
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::resolution::{
    ErasedResolvedExecution, ErasedResolvedTool, ResolvedToolInvocation, ToolFingerprint,
    ToolResolution,
};

/// 类型化工具执行 Future。
pub type ToolExecuteFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, ToolError>> + Send + 'a>>;

/// 已擦除具体输出类型的 `ToolResult` Future。
pub type ToolJsonFuture<'a> = Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;

/// 工具流式输出通道。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputChannel {
    /// 标准输出或普通文本进度。
    Stdout,
    /// 标准错误输出。
    Stderr,
}

/// 一段工具流式输出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutputChunk {
    /// 输出所属通道。
    pub channel: ToolOutputChannel,
    /// 面向观察者的增量文本。
    pub delta: String,
}

/// 流式输出回调；Core 会把它转换为 `AgentEvent::ToolOutput`。
pub type ToolOutputSink = Arc<dyn Fn(ToolOutputChunk) + Send + Sync>;

/// Core 调度同一模型 Turn 中工具调用时使用的执行属性。
///
/// 该属性只在工具注册边界冻结，不进入模型可见 [`ToolDefinition`]，也不属于
/// 授权事实。工具必须显式选择 [`ParallelEligible`](Self::ParallelEligible)；
/// 默认串行保证现有工具在升级后不改变副作用顺序。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolExecutionMode {
    /// 与批次中相邻调用按原顺序逐个执行。
    #[default]
    Serial,
    /// Core 可以把连续、同样具备该属性的调用组成并行组。
    ParallelEligible,
}

/// 单次工具执行的控制面上下文。
#[derive(Clone)]
pub struct ToolContext {
    /// 取消信号；取消不属于 [`ToolError`]。
    pub cancellation: CancellationToken,
    /// 执行期间发送 stdout/stderr 或进度片段的回调。
    pub output_sink: ToolOutputSink,
    /// 当前模型 Tool Call 的稳定标识；由 Core dispatch 时绑定。
    ///
    /// 直接单测工具时可以为空，只有需要建立跨层业务关系的工具才必须读取它。
    call_id: Option<ToolCallId>,
}

impl ToolContext {
    /// 用取消信号和流式输出回调创建工具上下文。
    pub fn new(cancellation: CancellationToken, output_sink: ToolOutputSink) -> Self {
        Self {
            cancellation,
            output_sink,
            call_id: None,
        }
    }

    /// 绑定当前调用标识。该信息不进入工具输入 Schema 或授权事实。
    #[must_use]
    pub fn with_call_id(mut self, call_id: ToolCallId) -> Self {
        self.call_id = Some(call_id);
        self
    }

    /// 读取由 Core 绑定的当前 Tool Call 标识。
    pub fn call_id(&self) -> Option<&ToolCallId> {
        self.call_id.as_ref()
    }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            output_sink: Arc::new(|_| {}),
            call_id: None,
        }
    }
}

/// 工具失败分类；两种失败最终都会转换为模型可见的错误 `ToolResult`。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ToolError {
    /// 输入校验失败或确定性 resolve 失败。
    #[error("invalid tool input: {message}")]
    InvalidInput {
        /// 模型可读的失败原因。
        message: String,
    },
    /// 工具真正开始执行后发生的失败。
    #[error("tool execution failed: {message}")]
    Execution {
        /// 模型可读的失败原因。
        message: String,
        /// 可选、受控的结构化错误细节。
        details: Option<Value>,
    },
}

impl ToolError {
    /// 创建输入或 resolve 错误。
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    /// 创建不含结构化细节的执行错误。
    pub fn execution(message: impl Into<String>) -> Self {
        Self::Execution {
            message: message.into(),
            details: None,
        }
    }

    /// 创建带受控结构化细节的执行错误。
    pub fn execution_with_details(message: impl Into<String>, details: Value) -> Self {
        Self::Execution {
            message: message.into(),
            details: Some(details),
        }
    }
}

/// 顶层输入属性的有序、模型可见默认值集合。
///
/// Registry 在注册工具前校验重复属性、未知属性和序列化失败。
/// 保留声明顺序可以让无效定义的报错稳定可重现。
#[derive(Clone, Debug, Default)]
pub struct ToolInputDefaults {
    entries: Vec<ToolInputDefault>,
}

#[derive(Clone, Debug)]
struct ToolInputDefault {
    property: String,
    value: Result<Value, String>,
}

impl ToolInputDefaults {
    /// 创建空默认值集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个可序列化的属性默认值。
    ///
    /// 序列化错误会先保留，再由 [`crate::ToolRegistry::register`] 统一返回，
    /// 从而让注册仍然是唯一的工具定义冻结边界。
    pub fn with<T: Serialize>(mut self, property: impl Into<String>, value: T) -> Self {
        self.entries.push(ToolInputDefault {
            property: property.into(),
            value: serde_json::to_value(value).map_err(|error| error.to_string()),
        });
        self
    }

    pub(crate) fn apply_to(self, input_schema: &mut Value) -> Result<(), String> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let properties = input_schema
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "input defaults require a top-level object schema".to_owned())?;
        let mut seen = HashSet::with_capacity(self.entries.len());
        for entry in self.entries {
            if !seen.insert(entry.property.clone()) {
                return Err(format!(
                    "input default for property `{}` is declared more than once",
                    entry.property
                ));
            }
            let value = entry.value.map_err(|message| {
                format!(
                    "input default for property `{}` cannot be serialized: {message}",
                    entry.property
                )
            })?;
            let property_schema = properties
                .get_mut(&entry.property)
                .ok_or_else(|| {
                    format!(
                        "input default references unknown property `{}`",
                        entry.property
                    )
                })?
                .as_object_mut()
                .ok_or_else(|| {
                    format!(
                        "input schema for property `{}` cannot carry a default",
                        entry.property
                    )
                })?;
            property_schema.insert("default".to_owned(), value);
        }
        Ok(())
    }
}

/// 类型化 Agent 工具契约。
pub trait Tool: Send + Sync + 'static {
    /// 模型提供的原始输入类型；Serde 反序列化同时完成结构校验。
    type Input: DeserializeOwned + JsonSchema;
    /// 无副作用 resolve 后得到、由 `execute` 一次性消费的输入类型。
    type ResolvedInput: Serialize + Send + 'static;
    /// 执行输出类型，最终序列化进入 [`ToolResultContent::Json`]。
    type Output: Serialize;

    /// 模型可见的工具名。
    fn name(&self) -> ToolName;

    /// 模型可见的工具描述；返回拥有值，使工具实例配置可被冻结进描述。
    fn description(&self) -> String;

    /// 写入派生输入 Schema 的模型可见默认值。
    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::default()
    }

    /// 返回只供 Core 调度使用的执行属性；注册时读取并冻结一次。
    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Serial
    }

    /// 在不执行 I/O 的前提下，落实确定性默认值并构造授权事实。
    fn resolve(&self, input: Self::Input)
    -> Result<ToolResolution<Self::ResolvedInput>, ToolError>;

    /// 执行一次已完成 resolve 的工具调用。
    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output>;

    /// 把成功输出编码为模型可见内容；普通工具默认使用 JSON。
    fn encode_output(output: Self::Output) -> Result<ToolResultContent, String> {
        serde_json::to_value(output)
            .map(ToolResultContent::Json)
            .map_err(|error| error.to_string())
    }

    /// 提取不进入模型上下文、但需要随可靠结果保存的执行观测信息。
    fn execution_metadata(_output: &Self::Output) -> Option<agent_types::ToolExecutionMetadata> {
        None
    }
}

pub(crate) trait ErasedTool: Send + Sync {
    fn resolve(&self, call: &ToolCall) -> Result<ErasedResolvedTool, ToolResult>;
}

pub(crate) struct TypedToolErasure<T: Tool> {
    tool: Arc<T>,
    execution_mode: ToolExecutionMode,
}

impl<T: Tool> TypedToolErasure<T> {
    pub(crate) fn new(tool: Arc<T>, execution_mode: ToolExecutionMode) -> Self {
        Self {
            tool,
            execution_mode,
        }
    }
}

impl<T: Tool> ErasedTool for TypedToolErasure<T> {
    fn resolve(&self, call: &ToolCall) -> Result<ErasedResolvedTool, ToolResult> {
        let input =
            serde_json::from_value::<T::Input>(call.arguments.clone()).map_err(|error| {
                text_error_result(
                    call,
                    format!("invalid arguments for tool `{}`: {error}", call.name),
                )
            })?;
        let resolution = self
            .tool
            .resolve(input)
            .map_err(|error| resolve_error_result(&call.id, error))?;
        let (resolved_input, authorization_facts, semantic_arguments) = resolution.into_parts();
        let resolved_arguments = serde_json::to_value(&resolved_input).map_err(|error| {
            text_error_result(
                call,
                format!(
                    "failed to serialize resolved input of tool `{}`: {error}",
                    call.name
                ),
            )
        })?;
        let fingerprint = ToolFingerprint::new(
            call.name.clone(),
            semantic_arguments.unwrap_or_else(|| resolved_arguments.clone()),
        );
        let invocation = ResolvedToolInvocation::new(
            call.id.clone(),
            call.name.clone(),
            resolved_arguments,
            authorization_facts.unwrap_or_else(|| {
                Arc::new(crate::GeneralAuthorizationFacts {
                    tool_name: call.name.clone(),
                })
            }),
            fingerprint,
            self.execution_mode,
        );
        let executor: Box<dyn ErasedResolvedExecution> = Box::new(TypedResolvedExecution {
            tool: self.tool.clone(),
            input: resolved_input,
            call_id: call.id.clone(),
        });
        Ok(ErasedResolvedTool {
            invocation,
            executor,
        })
    }
}

struct TypedResolvedExecution<T: Tool> {
    tool: Arc<T>,
    input: T::ResolvedInput,
    call_id: agent_types::ToolCallId,
}

impl<T: Tool> ErasedResolvedExecution for TypedResolvedExecution<T> {
    fn execute(self: Box<Self>, context: ToolContext) -> ToolJsonFuture<'static> {
        Box::pin(async move {
            let Self {
                tool,
                input,
                call_id,
            } = *self;
            match tool.execute(input, context).await {
                Ok(output) => {
                    let metadata = T::execution_metadata(&output);
                    match T::encode_output(output) {
                        Ok(content) => ToolResult {
                            call_id,
                            status: ToolResultStatus::Success,
                            content,
                            metadata: metadata.map(Box::new),
                        },
                        Err(error) => text_error_result_for_id(
                            call_id,
                            format!("failed to serialize tool output: {error}"),
                        ),
                    }
                }
                Err(error) => tool_error_result(&call_id, error),
            }
        })
    }
}

pub(crate) fn frozen_definition<T: Tool>(
    tool: &T,
    name: ToolName,
) -> Result<ToolDefinition, String> {
    let description = tool.description();
    let mut input_schema = input_schema_value::<T::Input>();
    tool.input_defaults().apply_to(&mut input_schema)?;
    Ok(ToolDefinition {
        name,
        description,
        input_schema,
    })
}

fn input_schema_value<Input: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(Input))
        .expect("schemars schema serialization is infallible")
}

pub(crate) fn text_error_result(call: &ToolCall, message: String) -> ToolResult {
    text_error_result_for_id(call.id.clone(), message)
}

pub(crate) fn text_error_result_for_id(
    call_id: agent_types::ToolCallId,
    message: String,
) -> ToolResult {
    ToolResult {
        call_id,
        status: ToolResultStatus::Error,
        content: ToolResultContent::Text(message),
        metadata: None,
    }
}

fn tool_error_result(call_id: &agent_types::ToolCallId, error: ToolError) -> ToolResult {
    match error {
        ToolError::InvalidInput { message } => {
            text_error_result_for_id(call_id.clone(), format!("invalid tool input: {message}"))
        }
        ToolError::Execution {
            message,
            details: None,
        } => text_error_result_for_id(call_id.clone(), format!("tool execution failed: {message}")),
        ToolError::Execution {
            message,
            details: Some(details),
        } => ToolResult {
            call_id: call_id.clone(),
            status: ToolResultStatus::Error,
            content: ToolResultContent::Json(json!({
                "error": {
                    "message": message,
                    "details": details,
                }
            })),
            metadata: None,
        },
    }
}

/// resolve 是无副作用输入阶段；即使某个 Tool 实现误用了 Execution，擦除边界也要把
/// 它归一化成输入错误，避免对外暴露错误的生命周期阶段。
fn resolve_error_result(call_id: &agent_types::ToolCallId, error: ToolError) -> ToolResult {
    let message = match error {
        ToolError::InvalidInput { message } | ToolError::Execution { message, .. } => message,
    };
    tool_error_result(call_id, ToolError::invalid_input(message))
}

#[cfg(test)]
mod tests {
    use serde::ser::{Error as _, Serializer};
    use serde_json::json;

    use super::*;

    struct CannotSerialize;

    impl Serialize for CannotSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("not representable"))
        }
    }

    #[test]
    fn defaults_reject_duplicates_unknown_properties_and_serialization_failures() {
        let schema = || {
            json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer"}
                }
            })
        };

        let mut duplicate_schema = schema();
        let duplicate = ToolInputDefaults::new()
            .with("limit", 10)
            .with("limit", 20)
            .apply_to(&mut duplicate_schema)
            .expect_err("duplicate default");
        assert!(duplicate.contains("more than once"));

        let mut unknown_schema = schema();
        let unknown = ToolInputDefaults::new()
            .with("missing", 10)
            .apply_to(&mut unknown_schema)
            .expect_err("unknown property");
        assert!(unknown.contains("unknown property"));

        let mut invalid_schema = schema();
        let invalid = ToolInputDefaults::new()
            .with("limit", CannotSerialize)
            .apply_to(&mut invalid_schema)
            .expect_err("serialization failure");
        assert!(invalid.contains("not representable"));
    }

    #[test]
    fn execution_details_use_stable_json_shape() {
        let call_id = agent_types::ToolCallId::new("call_1").expect("valid call id");
        let result = tool_error_result(
            &call_id,
            ToolError::execution_with_details(
                "timed out",
                json!({"type": "timeout", "truncated": false}),
            ),
        );
        assert_eq!(
            result.content,
            ToolResultContent::Json(json!({
                "error": {
                    "message": "timed out",
                    "details": {"type": "timeout", "truncated": false},
                }
            }))
        );
    }
}
