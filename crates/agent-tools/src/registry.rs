//! 工具注册表与不可变快照。
//!
//! [`ToolRegistry`] 在装配期注册工具（重名拒绝），构建完成后冻结为
//! [`ToolSetSnapshot`]；快照随 `ExecutionPlan` 进入执行，执行期间不变。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use agent_types::{ToolDefinition, ToolName};
use thiserror::Error;

use crate::tool::{ErasedTool, Tool, TypedToolErasure};

/// 注册工具失败。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RegisterToolError {
    /// 同名工具已经注册。
    #[error("tool `{0}` is already registered")]
    DuplicateName(ToolName),
}

/// 装配期工具注册表。
///
/// `ToolDefinition` 在注册时读取一次并与工具句柄共同冻结：重名检查、快照定义
/// 与名称索引都基于冻结定义，不受实现方后续动态变化影响。
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<(ToolDefinition, Arc<dyn ErasedTool>)>,
    names: HashSet<String>,
}

impl ToolRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册类型化工具；重名拒绝。
    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> Result<(), RegisterToolError> {
        self.register_erased(Arc::new(TypedToolErasure(tool)))
    }

    /// 注册已擦除的工具；重名拒绝。定义在注册时冻结，快照不再二次读取。
    pub fn register_erased(&mut self, tool: Arc<dyn ErasedTool>) -> Result<(), RegisterToolError> {
        let definition = tool.definition();
        if !self.names.insert(definition.name.as_str().to_owned()) {
            return Err(RegisterToolError::DuplicateName(definition.name));
        }
        self.tools.push((definition, tool));
        Ok(())
    }

    /// 冻结为不可变快照；定义顺序与注册顺序一致，直接消费注册时冻结的定义。
    pub fn snapshot(self) -> ToolSetSnapshot {
        let mut definitions = Vec::with_capacity(self.tools.len());
        let mut tools = Vec::with_capacity(self.tools.len());
        let mut by_name = HashMap::with_capacity(self.tools.len());
        for (index, (definition, tool)) in self.tools.into_iter().enumerate() {
            by_name.insert(definition.name.as_str().to_owned(), index);
            definitions.push(definition);
            tools.push(tool);
        }
        ToolSetSnapshot {
            definitions,
            tools,
            by_name,
        }
    }
}

/// 不可变工具集快照；空快照是合法输入（最小可执行 Agent 不含工具）。
#[derive(Clone, Default)]
pub struct ToolSetSnapshot {
    definitions: Vec<ToolDefinition>,
    tools: Vec<Arc<dyn ErasedTool>>,
    by_name: HashMap<String, usize>,
}

impl ToolSetSnapshot {
    /// 模型可见的工具定义列表，顺序与注册顺序一致。
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    /// 快照中的工具数量。
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// 快照是否为空。
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// 按名称查找已擦除的工具。
    pub(crate) fn tool(&self, name: &ToolName) -> Option<&Arc<dyn ErasedTool>> {
        self.by_name
            .get(name.as_str())
            .map(|index| &self.tools[*index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{AddTool, FailTool};

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = ToolRegistry::new();
        registry.register(AddTool).expect("first registration");
        let error = registry
            .register(AddTool)
            .expect_err("duplicate name must be rejected");
        assert_eq!(
            error,
            RegisterToolError::DuplicateName(ToolName::new("add").expect("valid tool name"))
        );
    }

    #[test]
    fn snapshot_preserves_registration_order() {
        let mut registry = ToolRegistry::new();
        registry.register(FailTool).expect("register fail tool");
        registry.register(AddTool).expect("register add tool");
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 2);
        let names: Vec<&str> = snapshot
            .definitions()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(names, ["fail", "add"]);
    }

    #[test]
    fn empty_snapshot_is_valid() {
        let snapshot = ToolRegistry::new().snapshot();
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.len(), 0);
        assert!(snapshot.definitions().is_empty());
        assert!(ToolSetSnapshot::default().is_empty());
    }

    /// definition() 每次返回不同名称的异常实现。
    struct ShiftingTool {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl crate::ErasedTool for ShiftingTool {
        fn definition(&self) -> ToolDefinition {
            let call_count = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let name = if call_count.is_multiple_of(2) {
                "even"
            } else {
                "odd"
            };
            ToolDefinition {
                name: ToolName::new(name).expect("valid tool name"),
                description: "Return a different name on every definition() call".to_owned(),
                input_schema: serde_json::Value::Null,
            }
        }

        fn execute_json<'a>(
            &'a self,
            call: &'a agent_types::ToolCall,
            _context: crate::ToolContext,
        ) -> crate::ToolJsonFuture<'a> {
            Box::pin(std::future::ready(agent_types::ToolResult {
                call_id: call.id.clone(),
                status: agent_types::ToolResultStatus::Success,
                content: agent_types::ToolResultContent::Json(serde_json::json!({"ok": true})),
            }))
        }
    }

    #[test]
    fn frozen_definition_survives_shifting_names() {
        let mut registry = ToolRegistry::new();
        // 注册时读取一次定义（"even"）并冻结；此后实现方返回什么不再重要。
        registry
            .register_erased(std::sync::Arc::new(ShiftingTool {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }))
            .expect("register shifting tool");

        // 重名检查以冻结名称为准：另一个首调用同样返回 "even" 的实现必须被拒绝。
        let error = registry
            .register_erased(std::sync::Arc::new(ShiftingTool {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }))
            .expect_err("frozen duplicate name must be rejected");
        assert_eq!(
            error,
            RegisterToolError::DuplicateName(ToolName::new("even").expect("valid tool name"))
        );

        // 快照只含冻结名称；按冻结名称可派发，按后续动态名称不可派发。
        let snapshot = registry.snapshot();
        let names: Vec<&str> = snapshot
            .definitions()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(names, ["even"]);

        let frozen = crate::testutil::tool_call("even", serde_json::json!({}));
        let result = crate::testutil::block_on(crate::Dispatcher::dispatch(
            &snapshot,
            &frozen,
            crate::ToolContext::default(),
        ));
        assert_eq!(result.status, agent_types::ToolResultStatus::Success);

        let shifted = crate::testutil::tool_call("odd", serde_json::json!({}));
        let result = crate::testutil::block_on(crate::Dispatcher::dispatch(
            &snapshot,
            &shifted,
            crate::ToolContext::default(),
        ));
        assert_eq!(result.status, agent_types::ToolResultStatus::Error);
    }
}
