//! 类型化工具注册表与不可变快照。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use agent_types::{ToolDefinition, ToolName};
use thiserror::Error;

use crate::tool::{ErasedTool, Tool, TypedToolErasure, frozen_definition};

/// 工具注册失败。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RegisterToolError {
    /// 相同冻结名称的工具已经存在。
    #[error("tool `{0}` is already registered")]
    DuplicateName(ToolName),
    /// 派生 Schema 与工具实例提供的默认值无法组成有效冻结定义。
    #[error("invalid definition for tool `{name}`: {message}")]
    InvalidDefinition {
        /// 在注册边界读取并冻结的工具名。
        name: ToolName,
        /// 稳定、可重现的定义校验失败原因。
        message: String,
    },
}

/// 装配阶段使用的类型化工具注册表。
///
/// 完成注册后通过 [`ToolRegistry::snapshot`] 消费注册表，进入执行期不可变快照。
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

    /// 注册一个类型化工具，并一次冻结名称、描述、Schema 和默认值。
    pub fn register<T: Tool>(&mut self, tool: T) -> Result<(), RegisterToolError> {
        let (definition, tool) = freeze_tool(tool)?;
        if !self.names.insert(definition.name.as_str().to_owned()) {
            return Err(RegisterToolError::DuplicateName(definition.name));
        }
        self.tools.push((definition, tool));
        Ok(())
    }

    /// 消费注册表，冻结注册顺序、工具定义、工具句柄和名称索引。
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

/// 执行期使用的不可变工具集快照；空快照也是合法输入。
#[derive(Clone, Default)]
pub struct ToolSetSnapshot {
    definitions: Vec<ToolDefinition>,
    tools: Vec<Arc<dyn ErasedTool>>,
    by_name: HashMap<String, usize>,
}

impl ToolSetSnapshot {
    /// 消费当前快照并在尾部追加一个新工具，生成派生快照。
    ///
    /// 已有定义、顺序和执行句柄保持不变；新工具仍经过与 Registry 相同的定义冻结和
    /// 重名校验。该接口用于上层从同一基础快照派生工具集，而无需解析或重建既有工具。
    ///
    /// # Errors
    ///
    /// 新工具定义无效或与已有冻结名称重复时返回 [`RegisterToolError`]。
    pub fn try_with_tool<T: Tool>(mut self, tool: T) -> Result<Self, RegisterToolError> {
        let (definition, tool) = freeze_tool(tool)?;
        if self.by_name.contains_key(definition.name.as_str()) {
            return Err(RegisterToolError::DuplicateName(definition.name));
        }
        let index = self.tools.len();
        self.by_name
            .insert(definition.name.as_str().to_owned(), index);
        self.definitions.push(definition);
        self.tools.push(tool);
        Ok(self)
    }

    /// 按注册顺序返回模型可见工具定义。
    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    /// 已冻结工具定义数量。
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// 是否不包含任何工具。
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    pub(crate) fn tool(&self, name: &ToolName) -> Option<&Arc<dyn ErasedTool>> {
        self.by_name
            .get(name.as_str())
            .map(|index| &self.tools[*index])
    }
}

fn freeze_tool<T: Tool>(
    tool: T,
) -> Result<(ToolDefinition, Arc<dyn ErasedTool>), RegisterToolError> {
    let name = tool.name();
    let definition = frozen_definition(&tool, name.clone()).map_err(|message| {
        RegisterToolError::InvalidDefinition {
            name: name.clone(),
            message,
        }
    })?;
    let execution_mode = tool.execution_mode();
    let tool = Arc::new(tool);
    Ok((
        definition,
        Arc::new(TypedToolErasure::new(tool, execution_mode)),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{
        Dispatcher, ResolvedBatchItemRef, ToolContext, ToolError, ToolExecuteFuture,
        ToolExecutionMode, ToolInputDefaults, ToolResolution,
        testutil::{AddTool, FailTool},
    };

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
    fn snapshot_preserves_registration_order_and_empty_snapshot_is_valid() {
        let mut registry = ToolRegistry::new();
        registry.register(FailTool).expect("register fail tool");
        registry.register(AddTool).expect("register add tool");
        let snapshot = registry.snapshot();
        let names: Vec<&str> = snapshot
            .definitions()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(names, ["fail", "add"]);

        let empty = ToolRegistry::new().snapshot();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.definitions().is_empty());
    }

    struct InvalidDefaultTool {
        defaults: ToolInputDefaults,
    }

    impl crate::Tool for InvalidDefaultTool {
        type Input = crate::testutil::AddInput;
        type ResolvedInput = crate::testutil::AddInput;
        type Output = crate::testutil::AddOutput;

        fn name(&self) -> ToolName {
            ToolName::new("invalid_default").expect("valid name")
        }

        fn description(&self) -> String {
            "invalid defaults".to_owned()
        }

        fn input_defaults(&self) -> ToolInputDefaults {
            self.defaults.clone()
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(crate::testutil::AddOutput {
                sum: input.a + input.b,
            })))
        }
    }

    #[test]
    fn invalid_defaults_do_not_partially_register() {
        let mut registry = ToolRegistry::new();
        let error = registry
            .register(InvalidDefaultTool {
                defaults: ToolInputDefaults::new().with("missing", 1),
            })
            .expect_err("unknown property must fail registration");
        assert!(matches!(error, RegisterToolError::InvalidDefinition { .. }));
        assert!(registry.snapshot().is_empty());
    }

    #[derive(Deserialize, JsonSchema, Serialize)]
    struct OptionalInput {
        limit: Option<u32>,
    }

    struct DefaultTool;

    impl crate::Tool for DefaultTool {
        type Input = OptionalInput;
        type ResolvedInput = OptionalInput;
        type Output = OptionalInput;

        fn name(&self) -> ToolName {
            ToolName::new("default_tool").expect("valid name")
        }

        fn description(&self) -> String {
            "default tool".to_owned()
        }

        fn input_defaults(&self) -> ToolInputDefaults {
            ToolInputDefaults::new().with("limit", 200_u32)
        }

        fn resolve(
            &self,
            mut input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            input.limit = Some(input.limit.unwrap_or(200));
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    #[test]
    fn definition_defaults_are_frozen_once() {
        let mut registry = ToolRegistry::new();
        registry
            .register(DefaultTool)
            .expect("register default tool");
        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot.definitions()[0].input_schema["properties"]["limit"]["default"],
            serde_json::json!(200)
        );
    }

    struct CountingDefinitionTool {
        name_calls: Arc<AtomicUsize>,
        description_calls: Arc<AtomicUsize>,
        defaults_calls: Arc<AtomicUsize>,
        execution_mode_calls: Arc<AtomicUsize>,
    }

    impl crate::Tool for CountingDefinitionTool {
        type Input = OptionalInput;
        type ResolvedInput = OptionalInput;
        type Output = OptionalInput;

        fn name(&self) -> ToolName {
            self.name_calls.fetch_add(1, Ordering::SeqCst);
            ToolName::new("counting_definition").expect("valid name")
        }

        fn description(&self) -> String {
            self.description_calls.fetch_add(1, Ordering::SeqCst);
            "frozen description".to_owned()
        }

        fn input_defaults(&self) -> ToolInputDefaults {
            self.defaults_calls.fetch_add(1, Ordering::SeqCst);
            ToolInputDefaults::new().with("limit", 10_u32)
        }

        fn execution_mode(&self) -> ToolExecutionMode {
            self.execution_mode_calls.fetch_add(1, Ordering::SeqCst);
            ToolExecutionMode::Serial
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    #[test]
    fn definition_components_are_read_once_at_registration() {
        let name_calls = Arc::new(AtomicUsize::new(0));
        let description_calls = Arc::new(AtomicUsize::new(0));
        let defaults_calls = Arc::new(AtomicUsize::new(0));
        let execution_mode_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(CountingDefinitionTool {
                name_calls: name_calls.clone(),
                description_calls: description_calls.clone(),
                defaults_calls: defaults_calls.clone(),
                execution_mode_calls: execution_mode_calls.clone(),
            })
            .expect("register counting tool");
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.definitions()[0].description, "frozen description");
        assert_eq!(snapshot.definitions()[0].description, "frozen description");
        assert_eq!(name_calls.load(Ordering::SeqCst), 1);
        assert_eq!(description_calls.load(Ordering::SeqCst), 1);
        assert_eq!(defaults_calls.load(Ordering::SeqCst), 1);
        assert_eq!(execution_mode_calls.load(Ordering::SeqCst), 1);
    }

    struct ParallelTool;

    impl crate::Tool for ParallelTool {
        type Input = OptionalInput;
        type ResolvedInput = OptionalInput;
        type Output = OptionalInput;

        fn name(&self) -> ToolName {
            ToolName::new("parallel_tool").expect("valid name")
        }

        fn description(&self) -> String {
            "parallel tool".to_owned()
        }

        fn execution_mode(&self) -> ToolExecutionMode {
            ToolExecutionMode::ParallelEligible
        }

        fn resolve(
            &self,
            input: Self::Input,
        ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
            Ok(ToolResolution::general(input))
        }

        fn execute<'a>(
            &'a self,
            input: Self::ResolvedInput,
            _context: ToolContext,
        ) -> ToolExecuteFuture<'a, Self::Output> {
            Box::pin(std::future::ready(Ok(input)))
        }
    }

    #[test]
    fn consuming_derivation_preserves_base_and_freezes_internal_execution_mode() {
        let mut registry = ToolRegistry::new();
        registry.register(AddTool).expect("register base tool");
        let base = registry.snapshot();
        let base_definition = base.definitions()[0].clone();

        let derived = base
            .clone()
            .try_with_tool(ParallelTool)
            .expect("derive tool set");
        assert_eq!(base.len(), 1);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived.definitions()[0], base_definition);
        assert_eq!(derived.definitions()[1].name.as_str(), "parallel_tool");

        let encoded = serde_json::to_value(derived.definitions()).expect("serialize definitions");
        assert!(
            !encoded.to_string().contains("execution_mode"),
            "execution mode must not enter provider-visible definitions"
        );

        let call = crate::testutil::tool_call("parallel_tool", serde_json::json!({"limit": 1}));
        let batch = Dispatcher::resolve_batch(&derived, &[call]);
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("parallel tool resolves");
        };
        assert_eq!(
            invocation.execution_mode(),
            ToolExecutionMode::ParallelEligible
        );

        let duplicate = match derived.try_with_tool(ParallelTool) {
            Ok(_) => panic!("derived snapshot must reject duplicate names"),
            Err(error) => error,
        };
        assert_eq!(
            duplicate,
            RegisterToolError::DuplicateName(ToolName::new("parallel_tool").expect("valid name"))
        );
    }
}
