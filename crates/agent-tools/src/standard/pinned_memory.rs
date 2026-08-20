//! Pinned Memory 标准工具壳。
//!
//! resolve 只校验领域输入和显式 Limits；Store 访问全部发生在 execute。修改 Store
//! 只影响未来新建会话，当前 AgentExecution 的 System Prompt 快照不会被刷新。

use std::{collections::BTreeMap, sync::Arc};

use agent_memory::{
    PinnedMemoryCategory, PinnedMemoryDraft, PinnedMemoryEntry, PinnedMemoryId, PinnedMemoryLimits,
    PinnedMemoryPatch, PinnedMemoryStore, PinnedMemoryStoreError,
};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};

/// `pin_memory` 的模型输入；稳定 ID 由 Store 原子分配。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinMemoryInput {
    /// 记忆的开放归类。
    pub category: PinnedMemoryCategory,
    /// 需要长期置顶的正文。
    pub content: String,
}

/// Pinned Memory 工具返回给模型的稳定内容投影。
///
/// `attributes` 是 Runtime 业务元数据，不属于模型可见记忆内容。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PinnedMemoryToolEntry {
    /// Store 分配的稳定 ID。
    pub id: PinnedMemoryId,
    /// 记忆的开放归类。
    pub category: PinnedMemoryCategory,
    /// 需要长期置顶的正文。
    pub content: String,
}

impl From<PinnedMemoryEntry> for PinnedMemoryToolEntry {
    fn from(entry: PinnedMemoryEntry) -> Self {
        Self {
            id: entry.id,
            category: entry.category,
            content: entry.content,
        }
    }
}

/// `pin_memory`：新增一条只影响未来新建会话的 Pinned Memory。
pub struct PinMemoryTool {
    store: Arc<dyn PinnedMemoryStore>,
    limits: PinnedMemoryLimits,
}

impl PinMemoryTool {
    /// 用 Store 能力和显式领域限制装配工具壳。
    pub fn new(store: Arc<dyn PinnedMemoryStore>, limits: PinnedMemoryLimits) -> Self {
        Self { store, limits }
    }
}

impl Tool for PinMemoryTool {
    type Input = PinMemoryInput;
    type ResolvedInput = PinnedMemoryDraft;
    type Output = PinnedMemoryToolEntry;

    fn name(&self) -> ToolName {
        ToolName::new("pin_memory").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Save a small, durable fact as pinned memory. The saved entry is returned immediately, \
         but the current session's system prompt snapshot does not change; the update applies to \
         future new sessions."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let draft = PinnedMemoryDraft {
            category: input.category,
            content: input.content,
            attributes: BTreeMap::new(),
        };
        draft
            .validate(&self.limits)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        Ok(ToolResolution::general(draft))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.store
                .pin(input, context.cancellation)
                .await
                .map(PinnedMemoryToolEntry::from)
                .map_err(map_store_error)
        })
    }
}

/// `update_pinned_memory` 的模型输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePinnedMemoryInput {
    /// 需要修改的稳定 ID。
    pub id: PinnedMemoryId,
    /// 新归类；省略时保持原值。
    pub category: Option<PinnedMemoryCategory>,
    /// 新正文；省略时保持原值。
    pub content: Option<String>,
}

/// `update_pinned_memory` resolve 后冻结的 ID 与 Patch。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedUpdatePinnedMemoryInput {
    /// 通过语法和容量校验的稳定 ID。
    pub id: PinnedMemoryId,
    /// 至少修改一项、并通过完整 Limits 校验的 Patch。
    pub patch: PinnedMemoryPatch,
}

/// `update_pinned_memory`：修改一条只影响未来新建会话的 Pinned Memory。
pub struct UpdatePinnedMemoryTool {
    store: Arc<dyn PinnedMemoryStore>,
    limits: PinnedMemoryLimits,
}

impl UpdatePinnedMemoryTool {
    /// 用 Store 能力和显式领域限制装配工具壳。
    pub fn new(store: Arc<dyn PinnedMemoryStore>, limits: PinnedMemoryLimits) -> Self {
        Self { store, limits }
    }
}

impl Tool for UpdatePinnedMemoryTool {
    type Input = UpdatePinnedMemoryInput;
    type ResolvedInput = ResolvedUpdatePinnedMemoryInput;
    type Output = PinnedMemoryToolEntry;

    fn name(&self) -> ToolName {
        ToolName::new("update_pinned_memory").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Update the category or content of an existing pinned memory. The returned entry is \
         current Store state, but this session's system prompt snapshot remains unchanged; the \
         update applies to future new sessions."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input
            .id
            .validate(&self.limits)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        let patch = PinnedMemoryPatch {
            category: input.category,
            content: input.content,
            attributes: None,
        };
        patch
            .validate(&self.limits)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        Ok(ToolResolution::general(ResolvedUpdatePinnedMemoryInput {
            id: input.id,
            patch,
        }))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.store
                .update(input.id, input.patch, context.cancellation)
                .await
                .map(PinnedMemoryToolEntry::from)
                .map_err(map_store_error)
        })
    }
}

/// `unpin_memory` 的模型输入。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnpinMemoryInput {
    /// 需要删除的稳定 ID。
    pub id: PinnedMemoryId,
}

/// `unpin_memory`：删除一条只影响未来新建会话的 Pinned Memory。
pub struct UnpinMemoryTool {
    store: Arc<dyn PinnedMemoryStore>,
    limits: PinnedMemoryLimits,
}

impl UnpinMemoryTool {
    /// 用 Store 能力和显式领域限制装配工具壳。
    pub fn new(store: Arc<dyn PinnedMemoryStore>, limits: PinnedMemoryLimits) -> Self {
        Self { store, limits }
    }
}

impl Tool for UnpinMemoryTool {
    type Input = UnpinMemoryInput;
    type ResolvedInput = UnpinMemoryInput;
    type Output = PinnedMemoryToolEntry;

    fn name(&self) -> ToolName {
        ToolName::new("unpin_memory").expect("valid tool name")
    }

    fn description(&self) -> String {
        "Remove a pinned memory and return the removed entry. The current session's system prompt \
         snapshot remains unchanged; removal applies to future new sessions."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input
            .id
            .validate(&self.limits)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        Ok(ToolResolution::general(input))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.store
                .unpin(input.id, context.cancellation)
                .await
                .map(PinnedMemoryToolEntry::from)
                .map_err(map_store_error)
        })
    }
}

/// `list_pinned_memories` 的空模型输入。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListPinnedMemoriesInput {}

/// `list_pinned_memories`：查看 Store 最新状态，不代表当前 Prompt 快照。
pub struct ListPinnedMemoriesTool {
    store: Arc<dyn PinnedMemoryStore>,
}

impl ListPinnedMemoriesTool {
    /// 用 Store 能力装配只读工具壳。
    pub fn new(store: Arc<dyn PinnedMemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for ListPinnedMemoriesTool {
    type Input = ListPinnedMemoriesInput;
    type ResolvedInput = ListPinnedMemoriesInput;
    type Output = Vec<PinnedMemoryToolEntry>;

    fn name(&self) -> ToolName {
        ToolName::new("list_pinned_memories").expect("valid tool name")
    }

    fn description(&self) -> String {
        "List the pinned memory Store's latest complete state without pagination. This may differ \
         from the pinned memory snapshot frozen into the current session's system prompt."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        Ok(ToolResolution::general(input))
    }

    fn execute<'a>(
        &'a self,
        _input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.store
                .list(context.cancellation)
                .await
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(PinnedMemoryToolEntry::from)
                        .collect()
                })
                .map_err(map_store_error)
        })
    }
}

fn map_store_error(error: PinnedMemoryStoreError) -> ToolError {
    match error {
        PinnedMemoryStoreError::InvalidInput { message } => ToolError::invalid_input(message),
        other => ToolError::execution(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use agent_memory::{MemoryPropertyValue, PinnedMemoryFuture, PinnedMemoryValidationError};
    use agent_types::{ToolResultContent, ToolResultStatus};
    use serde_json::json;

    use super::*;
    use crate::{
        Dispatcher, ToolRegistry,
        testutil::{block_on, tool_call},
    };

    struct ProbeStore {
        entries: Mutex<Vec<PinnedMemoryEntry>>,
        calls: Mutex<Vec<&'static str>>,
        next_id: AtomicUsize,
    }

    impl ProbeStore {
        fn new(entries: Vec<PinnedMemoryEntry>) -> Self {
            Self {
                entries: Mutex::new(entries),
                calls: Mutex::new(Vec::new()),
                next_id: AtomicUsize::new(1),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("lock calls").clone()
        }
    }

    impl PinnedMemoryStore for ProbeStore {
        fn list(
            &self,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                self.calls.lock().expect("lock calls").push("list");
                Ok(self.entries.lock().expect("lock entries").clone())
            })
        }

        fn pin(
            &self,
            draft: PinnedMemoryDraft,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                self.calls.lock().expect("lock calls").push("pin");
                let entry = PinnedMemoryEntry {
                    id: PinnedMemoryId::new(format!(
                        "pinned_{}",
                        self.next_id.fetch_add(1, Ordering::SeqCst)
                    ))
                    .expect("valid generated id"),
                    category: draft.category,
                    content: draft.content,
                    attributes: draft.attributes,
                };
                self.entries
                    .lock()
                    .expect("lock entries")
                    .push(entry.clone());
                Ok(entry)
            })
        }

        fn update(
            &self,
            id: PinnedMemoryId,
            patch: PinnedMemoryPatch,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                self.calls.lock().expect("lock calls").push("update");
                let mut entries = self.entries.lock().expect("lock entries");
                let entry = entries
                    .iter_mut()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| PinnedMemoryStoreError::NotFound { id: id.clone() })?;
                if let Some(category) = patch.category {
                    entry.category = category;
                }
                if let Some(content) = patch.content {
                    entry.content = content;
                }
                if let Some(attributes) = patch.attributes {
                    entry.attributes = attributes;
                }
                Ok(entry.clone())
            })
        }

        fn unpin(
            &self,
            id: PinnedMemoryId,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(PinnedMemoryStoreError::Cancelled);
                }
                self.calls.lock().expect("lock calls").push("unpin");
                let mut entries = self.entries.lock().expect("lock entries");
                let index = entries
                    .iter()
                    .position(|entry| entry.id == id)
                    .ok_or(PinnedMemoryStoreError::NotFound { id })?;
                Ok(entries.remove(index))
            })
        }
    }

    struct FailingStore {
        error: PinnedMemoryStoreError,
    }

    impl PinnedMemoryStore for FailingStore {
        fn list(
            &self,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, Vec<PinnedMemoryEntry>> {
            let error = self.error.clone();
            Box::pin(async move { Err(error) })
        }

        fn pin(
            &self,
            _draft: PinnedMemoryDraft,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            let error = self.error.clone();
            Box::pin(async move { Err(error) })
        }

        fn update(
            &self,
            _id: PinnedMemoryId,
            _patch: PinnedMemoryPatch,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            let error = self.error.clone();
            Box::pin(async move { Err(error) })
        }

        fn unpin(
            &self,
            _id: PinnedMemoryId,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> PinnedMemoryFuture<'_, PinnedMemoryEntry> {
            let error = self.error.clone();
            Box::pin(async move { Err(error) })
        }
    }

    fn limits() -> PinnedMemoryLimits {
        PinnedMemoryLimits {
            max_entries: NonZeroUsize::new(8).expect("non-zero"),
            max_id_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_category_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_content_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_attributes_per_entry: NonZeroUsize::new(8).expect("non-zero"),
            max_attribute_key_bytes: NonZeroUsize::new(32).expect("non-zero"),
            max_attribute_string_bytes: NonZeroUsize::new(64).expect("non-zero"),
            max_description_bytes: NonZeroUsize::new(256).expect("non-zero"),
            max_snapshot_bytes: NonZeroUsize::new(4096).expect("non-zero"),
        }
    }

    fn entry(id: &str, content: &str) -> PinnedMemoryEntry {
        PinnedMemoryEntry {
            id: PinnedMemoryId::new(id).expect("valid id"),
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: content.to_owned(),
            attributes: BTreeMap::new(),
        }
    }

    fn execute(
        registry: ToolRegistry,
        name: &str,
        arguments: serde_json::Value,
        context: ToolContext,
    ) -> agent_types::ToolResult {
        let call = tool_call(name, arguments);
        let mut batch = Dispatcher::resolve_batch(&registry.snapshot(), &[call]);
        block_on(Dispatcher::execute(&mut batch, 0, context).expect("valid batch index"))
    }

    #[test]
    fn pinned_tool_definitions_have_fixed_names_schemas_and_snapshot_descriptions() {
        let store = Arc::new(ProbeStore::new(vec![]));
        let mut registry = ToolRegistry::new();
        registry
            .register(PinMemoryTool::new(store.clone(), limits()))
            .expect("register pin");
        registry
            .register(UpdatePinnedMemoryTool::new(store.clone(), limits()))
            .expect("register update");
        registry
            .register(UnpinMemoryTool::new(store.clone(), limits()))
            .expect("register unpin");
        registry
            .register(ListPinnedMemoriesTool::new(store.clone()))
            .expect("register list");
        let definitions = registry.snapshot().definitions().to_vec();
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "pin_memory",
                "update_pinned_memory",
                "unpin_memory",
                "list_pinned_memories"
            ]
        );
        assert!(
            definitions
                .iter()
                .all(|definition| definition.description.contains("session")
                    && definition.description.contains("system prompt"))
        );
        assert_eq!(
            definitions[0].input_schema["required"],
            json!(["category", "content"])
        );
        assert_eq!(definitions[3].input_schema["type"], json!("object"));
        assert_eq!(
            definitions[3].input_schema["additionalProperties"],
            json!(false)
        );
        assert!(definitions[3].input_schema.get("required").is_none());

        let mut duplicate = ToolRegistry::new();
        duplicate
            .register(ListPinnedMemoriesTool::new(store.clone()))
            .expect("first list");
        assert!(
            duplicate
                .register(ListPinnedMemoriesTool::new(store))
                .is_err()
        );
    }

    #[test]
    fn pinned_resolve_validates_without_touching_store() {
        let store = Arc::new(ProbeStore::new(vec![]));
        let pin = PinMemoryTool::new(store.clone(), limits());
        pin.resolve(PinMemoryInput {
            category: PinnedMemoryCategory::new("preference").expect("valid category"),
            content: "Use dark mode".to_owned(),
        })
        .expect("valid pin resolves");
        let update = UpdatePinnedMemoryTool::new(store.clone(), limits());
        assert!(matches!(
            update.resolve(UpdatePinnedMemoryInput {
                id: PinnedMemoryId::new("pinned_1").expect("valid id"),
                category: None,
                content: None,
            }),
            Err(ToolError::InvalidInput { .. })
        ));
        let unpin = UnpinMemoryTool::new(store.clone(), limits());
        assert!(matches!(
            unpin.resolve(UnpinMemoryInput {
                id: PinnedMemoryId::new("x".repeat(33)).expect("valid text")
            }),
            Err(ToolError::InvalidInput { .. })
        ));
        ListPinnedMemoriesTool::new(store.clone())
            .resolve(ListPinnedMemoriesInput {})
            .expect("list resolves");
        assert!(store.calls().is_empty());
    }

    #[test]
    fn pinned_tools_execute_typed_store_lifecycle_and_return_json() {
        let store = Arc::new(ProbeStore::new(vec![]));
        let mut registry = ToolRegistry::new();
        registry
            .register(PinMemoryTool::new(store.clone(), limits()))
            .expect("register pin");
        let pin = execute(
            registry,
            "pin_memory",
            json!({
                "category": "preference",
                "content": "Use dark mode"
            }),
            ToolContext::default(),
        );
        assert_eq!(pin.status, ToolResultStatus::Success);
        assert_eq!(
            pin.content,
            ToolResultContent::json(json!({
                "id": "pinned_1",
                "category": "preference",
                "content": "Use dark mode"
            }))
        );

        let mut registry = ToolRegistry::new();
        registry
            .register(UpdatePinnedMemoryTool::new(store.clone(), limits()))
            .expect("register update");
        assert_eq!(
            execute(
                registry,
                "update_pinned_memory",
                json!({"id": "pinned_1", "content": "Use light mode"}),
                ToolContext::default(),
            )
            .status,
            ToolResultStatus::Success
        );

        let mut registry = ToolRegistry::new();
        registry
            .register(ListPinnedMemoriesTool::new(store.clone()))
            .expect("register list");
        let list = execute(
            registry,
            "list_pinned_memories",
            json!({}),
            ToolContext::default(),
        );
        assert_eq!(list.status, ToolResultStatus::Success);
        assert_eq!(
            list.content,
            ToolResultContent::json(json!([{
                "id": "pinned_1",
                "category": "preference",
                "content": "Use light mode"
            }]))
        );

        let mut registry = ToolRegistry::new();
        registry
            .register(UnpinMemoryTool::new(store.clone(), limits()))
            .expect("register unpin");
        assert_eq!(
            execute(
                registry,
                "unpin_memory",
                json!({"id": "pinned_1"}),
                ToolContext::default(),
            )
            .status,
            ToolResultStatus::Success
        );
        assert_eq!(store.calls(), vec!["pin", "update", "list", "unpin"]);
    }

    #[test]
    fn pinned_store_not_found_capacity_and_cancel_map_to_error_results() {
        let store = Arc::new(ProbeStore::new(vec![entry("pinned_1", "one")]));
        let mut registry = ToolRegistry::new();
        registry
            .register(UpdatePinnedMemoryTool::new(store, limits()))
            .expect("register update");
        let missing = execute(
            registry,
            "update_pinned_memory",
            json!({"id": "missing", "content": "new"}),
            ToolContext::default(),
        );
        assert_eq!(missing.status, ToolResultStatus::Error);
        assert!(
            missing
                .content
                .as_single_text()
                .is_some_and(|message| message.contains("not found"))
        );

        let mut registry = ToolRegistry::new();
        registry
            .register(PinMemoryTool::new(
                Arc::new(FailingStore {
                    error: PinnedMemoryStoreError::CapacityExceeded {
                        message: "store is full".to_owned(),
                    },
                }),
                limits(),
            ))
            .expect("register pin");
        let capacity = execute(
            registry,
            "pin_memory",
            json!({"category": "preference", "content": "new"}),
            ToolContext::default(),
        );
        assert_eq!(capacity.status, ToolResultStatus::Error);
        assert!(
            capacity
                .content
                .as_single_text()
                .is_some_and(|message| message.contains("capacity exceeded"))
        );

        let store = Arc::new(ProbeStore::new(vec![]));
        let mut registry = ToolRegistry::new();
        registry
            .register(ListPinnedMemoriesTool::new(store))
            .expect("register list");
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let cancelled = execute(
            registry,
            "list_pinned_memories",
            json!({}),
            ToolContext::new(cancellation, Arc::new(|_| {})),
        );
        assert_eq!(cancelled.status, ToolResultStatus::Error);
        assert!(
            cancelled
                .content
                .as_single_text()
                .is_some_and(|message| message.contains("cancelled"))
        );
    }

    #[test]
    fn standalone_id_validation_reports_the_domain_limit() {
        let error = PinnedMemoryId::new("x".repeat(33))
            .expect("valid syntax")
            .validate(&limits())
            .expect_err("id exceeds limit");
        assert!(matches!(
            error,
            PinnedMemoryValidationError::TooLong {
                field: "pinned memory id",
                ..
            }
        ));
    }

    #[test]
    fn pinned_tool_inputs_reject_runtime_business_attributes() {
        let store = Arc::new(ProbeStore::new(vec![]));
        let mut registry = ToolRegistry::new();
        registry
            .register(PinMemoryTool::new(store.clone(), limits()))
            .expect("register pin");
        let pin = execute(
            registry,
            "pin_memory",
            json!({
                "category": "preference",
                "content": "Use dark mode",
                "attributes": {"source": "agent"}
            }),
            ToolContext::default(),
        );
        assert_eq!(pin.status, ToolResultStatus::Error);
        assert!(store.calls().is_empty());

        let mut registry = ToolRegistry::new();
        registry
            .register(UpdatePinnedMemoryTool::new(store.clone(), limits()))
            .expect("register update");
        let update = execute(
            registry,
            "update_pinned_memory",
            json!({
                "id": "pinned_1",
                "attributes": {"source": "agent"}
            }),
            ToolContext::default(),
        );
        assert_eq!(update.status, ToolResultStatus::Error);
        assert!(store.calls().is_empty());
    }

    #[test]
    fn pinned_tools_preserve_business_attributes_without_exposing_them() {
        let mut existing = entry("pinned_1", "Use dark mode");
        existing.attributes.insert(
            "source".to_owned(),
            MemoryPropertyValue::String("desktop".to_owned()),
        );
        let store = Arc::new(ProbeStore::new(vec![existing]));

        let mut registry = ToolRegistry::new();
        registry
            .register(UpdatePinnedMemoryTool::new(store.clone(), limits()))
            .expect("register update");
        let updated = execute(
            registry,
            "update_pinned_memory",
            json!({"id": "pinned_1", "content": "Use light mode"}),
            ToolContext::default(),
        );
        assert_eq!(updated.status, ToolResultStatus::Success);
        assert_eq!(
            updated.content,
            ToolResultContent::json(json!({
                "id": "pinned_1",
                "category": "preference",
                "content": "Use light mode"
            }))
        );
        assert_eq!(
            store.entries.lock().expect("lock entries")[0]
                .attributes
                .get("source"),
            Some(&MemoryPropertyValue::String("desktop".to_owned()))
        );

        let mut registry = ToolRegistry::new();
        registry
            .register(ListPinnedMemoriesTool::new(store))
            .expect("register list");
        let listed = execute(
            registry,
            "list_pinned_memories",
            json!({}),
            ToolContext::default(),
        );
        assert_eq!(listed.status, ToolResultStatus::Success);
        assert!(
            !serde_json::to_string(&listed.content)
                .expect("serialize tool content")
                .contains("attributes")
        );
    }
}
