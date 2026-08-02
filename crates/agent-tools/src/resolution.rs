//! resolved 工具调用的公开事实与 crate 内部一次性执行状态。
//!
//! 策略、Authorizer、审计和 Guardrail 只能读取冻结后的公开描述；
//! 真正的 `ResolvedInput` 只保存在 crate-private 一次性 executor 中。

use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use agent_types::{ToolCallId, ToolName, ToolResult};
use serde_json::Value;

use crate::{ToolContext, ToolJsonFuture};

/// resolved invocation 对外暴露的类型安全授权事实。
///
/// 调用方通过 [`ResolvedToolInvocation::facts`] 按具体类型读取，
/// 类型不匹配时得到 `None`，不会发生强制转换。
pub trait ToolAuthorizationFacts: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Send + Sync> ToolAuthorizationFacts for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// 普通、非专用工具使用的通用授权事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneralAuthorizationFacts {
    /// resolve 时冻结的工具名。
    pub tool_name: ToolName,
}

/// 用于“重复调用”检测的稳定语义指纹。
///
/// 指纹可以排除 timeout 等不改变主要操作语义的参数，因此不必与
/// `resolved_arguments` 完全相同。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolFingerprint {
    tool_name: ToolName,
    semantic_arguments: Arc<Value>,
}

impl ToolFingerprint {
    pub(crate) fn new(tool_name: ToolName, semantic_arguments: Value) -> Self {
        Self {
            tool_name,
            semantic_arguments: Arc::new(semantic_arguments),
        }
    }

    /// 冻结的工具名。
    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    /// 会影响重复语义判定的 resolved 参数。
    pub fn semantic_arguments(&self) -> &Value {
        &self.semantic_arguments
    }
}

/// 供策略、Authorizer、审计和 Guardrail 共同使用的公开不可变调用描述。
///
/// 该类型不包含可执行句柄，也不公开真正的类型化 `ResolvedInput`。
#[derive(Clone)]
pub struct ResolvedToolInvocation {
    call_id: ToolCallId,
    tool_name: ToolName,
    resolved_arguments: Arc<Value>,
    authorization_facts: Arc<dyn ToolAuthorizationFacts>,
    fingerprint: ToolFingerprint,
}

impl std::fmt::Debug for ResolvedToolInvocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedToolInvocation")
            .field("call_id", &self.call_id)
            .field("tool_name", &self.tool_name)
            .field("resolved_arguments", &self.resolved_arguments)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl ResolvedToolInvocation {
    pub(crate) fn new(
        call_id: ToolCallId,
        tool_name: ToolName,
        resolved_arguments: Value,
        authorization_facts: Arc<dyn ToolAuthorizationFacts>,
        fingerprint: ToolFingerprint,
    ) -> Self {
        Self {
            call_id,
            tool_name,
            resolved_arguments: Arc::new(resolved_arguments),
            authorization_facts,
            fingerprint,
        }
    }

    /// 模型协议中的原始 Tool Call ID。
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }

    /// 冻结的工具名。
    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    /// 默认值已落实的完整参数，供模型展示、授权和审计使用。
    pub fn resolved_arguments(&self) -> &Value {
        &self.resolved_arguments
    }

    /// 仅在请求的具体类型 `F` 与实际事实类型一致时返回引用。
    pub fn facts<F: ToolAuthorizationFacts>(&self) -> Option<&F> {
        self.authorization_facts
            .as_ref()
            .as_any()
            .downcast_ref::<F>()
    }

    /// 用于语义重复比较的稳定指纹。
    pub fn fingerprint(&self) -> &ToolFingerprint {
        &self.fingerprint
    }
}

/// 类型化 [`crate::Tool`] 在无副作用 resolve 阶段产生的中间结果。
///
/// `Input` 是已补齐默认值并可直接交给 `execute` 的 `ResolvedInput`。
pub struct ToolResolution<Input> {
    input: Input,
    authorization_facts: Option<Arc<dyn ToolAuthorizationFacts>>,
    semantic_arguments: Option<Value>,
}

impl<Input: std::fmt::Debug> std::fmt::Debug for ToolResolution<Input> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResolution")
            .field("input", &self.input)
            .field("semantic_arguments", &self.semantic_arguments)
            .finish_non_exhaustive()
    }
}

impl<Input> ToolResolution<Input> {
    /// 为普通工具创建解析结果；使用默认通用事实，并把完整 resolved
    /// 参数作为语义指纹。
    pub fn general(input: Input) -> Self {
        Self {
            input,
            authorization_facts: None,
            semantic_arguments: None,
        }
    }

    /// 使用显式类型化授权事实和显式语义指纹参数创建解析结果。
    pub fn with_facts<F>(input: Input, authorization_facts: F, semantic_arguments: Value) -> Self
    where
        F: ToolAuthorizationFacts,
    {
        Self {
            input,
            authorization_facts: Some(Arc::new(authorization_facts)),
            semantic_arguments: Some(semantic_arguments),
        }
    }

    /// 消费解析结果并取回类型化 resolved input。
    ///
    /// 该方法主要供工具直接单测使用；Registry/Dispatcher 调用方应执行 batch。
    pub fn into_input(self) -> Input {
        self.input
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Input,
        Option<Arc<dyn ToolAuthorizationFacts>>,
        Option<Value>,
    ) {
        (
            self.input,
            self.authorization_facts,
            self.semantic_arguments,
        )
    }
}

pub(crate) trait ErasedResolvedExecution: Send {
    fn execute(self: Box<Self>, context: ToolContext) -> ToolJsonFuture<'static>;
}

pub(crate) struct ErasedResolvedTool {
    pub(crate) invocation: ResolvedToolInvocation,
    pub(crate) executor: Box<dyn ErasedResolvedExecution>,
}

pub(crate) enum ResolvedBatchItem {
    Valid {
        /// 可供策略和授权读取的公开冻结描述。
        invocation: ResolvedToolInvocation,
        /// 一次性执行器。`Mutex` 只用于让整个 batch 可被 Send future
        /// 安全持有只读引用，不表示允许并发执行同一位置。
        executor: Mutex<Option<Box<dyn ErasedResolvedExecution>>>,
    },
    /// 未知工具、参数无效或 resolve 失败产生的稳定错误结果。
    Invalid(ToolResult),
}

/// resolved batch 中单个位置的只读视图。
#[derive(Clone, Copy, Debug)]
pub enum ResolvedBatchItemRef<'a> {
    /// 解析成功，可进入策略、授权和执行。
    Valid(&'a ResolvedToolInvocation),
    /// 未知工具、输入无效或 resolve 失败，不得进入授权和执行。
    Invalid(&'a ToolResult),
}

/// 保留原 Tool Call 数量与顺序的完整 resolved batch。
pub struct ResolvedToolBatch {
    pub(crate) items: Vec<ResolvedBatchItem>,
}

impl ResolvedToolBatch {
    /// 批次位置数，与原 Tool Call 数量相同。
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// 批次是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 按原始位置返回一个只读项视图。
    pub fn get(&self, index: usize) -> Option<ResolvedBatchItemRef<'_>> {
        self.items.get(index).map(|item| match item {
            ResolvedBatchItem::Valid { invocation, .. } => ResolvedBatchItemRef::Valid(invocation),
            ResolvedBatchItem::Invalid(result) => ResolvedBatchItemRef::Invalid(result),
        })
    }

    /// 按原 Tool Call 顺序迭代所有位置。
    pub fn iter(&self) -> impl ExactSizeIterator<Item = ResolvedBatchItemRef<'_>> {
        self.items.iter().map(|item| match item {
            ResolvedBatchItem::Valid { invocation, .. } => ResolvedBatchItemRef::Valid(invocation),
            ResolvedBatchItem::Invalid(result) => ResolvedBatchItemRef::Invalid(result),
        })
    }
}

pub(crate) fn ready_result(result: ToolResult) -> ToolJsonFuture<'static> {
    Box::pin(std::future::ready(result))
}

#[cfg(test)]
mod tests {
    use super::ResolvedToolBatch;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn resolved_batch_can_be_observed_by_a_send_authorization_future() {
        assert_send_sync::<ResolvedToolBatch>();
    }
}
