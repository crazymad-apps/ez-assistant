//! Memory Demo 的本地数据、记忆工具和 Provider 装配。

use std::{collections::BTreeMap, num::NonZeroUsize, path::Path, sync::Arc, time::Duration};

use agent_context::ContextWindowEvaluator;
use agent_core::{ExecutionBudget, ExecutionSpec, ModelRequestConfig};
use agent_memory::{
    CoordinatedMemoryRecall, CoordinatedMemoryRecallConfig, MemoryPropertyValue, MemoryRecall,
    PinnedMemoryLimits, PinnedMemoryStore, RecallSource, RecallSourceError, RecallSourceFuture,
    RecallSourceId, RecallSourceRequest, RecallSourceResponse,
};
use agent_model::{
    GenerationConfig, ModelService, ProviderOptions, ReasoningConfig, SystemPromptSnapshot,
};
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use agent_tools::{
    ListPinnedMemoriesTool, PinMemoryTool, RecallMemoryTool, RecallMemoryToolConfig, ToolRegistry,
    ToolSetSnapshot, UnpinMemoryTool, UpdatePinnedMemoryTool,
};
use agent_types::ToolChoice;
use tokio_util::sync::CancellationToken;

use crate::{
    DemoError,
    atomic_json::AtomicJsonWriter,
    config::ChatConfig,
    pinned_store::DemoPinnedMemoryStore,
    recall_source::{DemoRecallFile, DemoRecallRecord, DemoRecallSource},
};

pub(crate) const PINNED_FILE: &str = "pinned-memory.json";
pub(crate) const RECALL_FILE: &str = "recall-records.json";
pub(crate) const SESSIONS_DIR: &str = "sessions";
pub(crate) const DEMO_SOURCE_ID: &str = "demo_records";
pub(crate) const FAILING_SOURCE_ID: &str = "failing_demo";

pub(crate) struct DemoMemoryResources {
    pub(crate) store: Arc<DemoPinnedMemoryStore>,
    pub(crate) tools: ToolSetSnapshot,
    pub(crate) limits: PinnedMemoryLimits,
}

pub(crate) struct ChatResources {
    pub(crate) memory: DemoMemoryResources,
    pub(crate) model: Arc<dyn ModelService>,
    pub(crate) context_window: Arc<ContextWindowEvaluator>,
    pub(crate) model_request: ModelRequestConfig,
}

impl ChatResources {
    pub(crate) fn spec(&self, system_prompt: SystemPromptSnapshot) -> ExecutionSpec {
        ExecutionSpec {
            system_prompt,
            model: Arc::clone(&self.model),
            context_window: Arc::clone(&self.context_window),
            tools: self.memory.tools.clone(),
            model_request: self.model_request.clone(),
            budget: ExecutionBudget {
                max_steps: Some(12),
                max_tool_calls: Some(24),
            },
            guardrails: None,
        }
    }
}

pub(crate) async fn build_chat_resources(
    data_dir: &Path,
    config: ChatConfig,
) -> Result<ChatResources, DemoError> {
    let memory = build_memory_resources(data_dir).await?;
    let model = OpenAiCompatibleService::new(
        config.base_url,
        BearerCredential::new(config.api_key),
        config.model,
        config.context_window_tokens,
        Profile::deepseek(),
        TransportTimeouts::default(),
    )
    .map_err(|error| DemoError::Provider(error.to_string()))?;
    Ok(ChatResources {
        memory,
        model: Arc::new(model),
        context_window: Arc::new(
            ContextWindowEvaluator::new(1.0)
                .map_err(|error| DemoError::Config(error.to_string()))?,
        ),
        model_request: deepseek_model_request_config(),
    })
}

fn deepseek_model_request_config() -> ModelRequestConfig {
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert(
            "deepseek",
            serde_json::json!({"thinking": {"type": "enabled"}}),
        )
        .expect("static DeepSeek provider options are valid");
    ModelRequestConfig {
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: Some(ReasoningConfig { effort: None }),
        provider_options,
    }
}

pub(crate) async fn build_memory_resources(
    data_dir: &Path,
) -> Result<DemoMemoryResources, DemoError> {
    tokio::fs::create_dir_all(data_dir)
        .await
        .map_err(|error| DemoError::Io(error.to_string()))?;
    tokio::fs::create_dir_all(data_dir.join(SESSIONS_DIR))
        .await
        .map_err(|error| DemoError::Io(error.to_string()))?;
    initialize_recall_file(&data_dir.join(RECALL_FILE)).await?;

    let limits = pinned_limits();
    let store = DemoPinnedMemoryStore::open(data_dir.join(PINNED_FILE), limits.clone())
        .await
        .map_err(|error| DemoError::Memory(error.to_string()))?;
    let demo_source_id = RecallSourceId::new(DEMO_SOURCE_ID)
        .map_err(|error| DemoError::Memory(error.to_string()))?;
    let failing_source_id = RecallSourceId::new(FAILING_SOURCE_ID)
        .map_err(|error| DemoError::Memory(error.to_string()))?;
    let sources: Vec<Arc<dyn RecallSource>> = vec![
        Arc::new(DemoRecallSource::new(
            demo_source_id.clone(),
            data_dir.join(RECALL_FILE),
            non_zero(4096),
        )),
        Arc::new(FailingDemoSource {
            id: failing_source_id,
        }),
    ];
    let recall: Arc<dyn MemoryRecall> = Arc::new(
        CoordinatedMemoryRecall::new(
            sources,
            CoordinatedMemoryRecallConfig {
                default_sources: vec![demo_source_id],
                source_timeout: Duration::from_secs(5),
                max_sources: non_zero(2),
                max_query_bytes: non_zero(1024),
                max_source_id_bytes: non_zero(64),
                max_item_bytes: non_zero(4096),
            },
        )
        .map_err(|error| DemoError::Memory(error.to_string()))?,
    );
    let store_capability: Arc<dyn PinnedMemoryStore> = store.clone();
    let mut registry = ToolRegistry::new();
    registry
        .register(PinMemoryTool::new(
            Arc::clone(&store_capability),
            limits.clone(),
        ))
        .map_err(tool_registration_error)?;
    registry
        .register(UpdatePinnedMemoryTool::new(
            Arc::clone(&store_capability),
            limits.clone(),
        ))
        .map_err(tool_registration_error)?;
    registry
        .register(UnpinMemoryTool::new(
            Arc::clone(&store_capability),
            limits.clone(),
        ))
        .map_err(tool_registration_error)?;
    registry
        .register(ListPinnedMemoriesTool::new(store_capability))
        .map_err(tool_registration_error)?;
    registry
        .register(RecallMemoryTool::new(
            recall,
            RecallMemoryToolConfig::new(non_zero(20)),
        ))
        .map_err(tool_registration_error)?;

    Ok(DemoMemoryResources {
        store,
        tools: registry.snapshot(),
        limits,
    })
}

pub(crate) fn new_session_prompt(id: String) -> crate::session::NewSessionInput {
    crate::session::NewSessionInput {
        id,
        instruction_parts: vec![
            "You are running inside the ez-assistant Memory Demo. Use memory tools only when they \
             help fulfill the user's request."
                .to_owned(),
            "Pinned memory is a small durable Store whose snapshot is frozen when this session is \
             created. pin/update/unpin affect future new sessions, not this session's system prompt."
                .to_owned(),
        ],
        recall_part: format!(
            "recall_memory searches larger historical or external data on demand. `{DEMO_SOURCE_ID}` \
             is the local demo record source and the default when sources is omitted. \
             `{FAILING_SOURCE_ID}` intentionally returns Unavailable for failure testing. Recall \
             results are temporary tool context and are never automatically pinned."
        ),
        pinned_description: "These durable entries were frozen when this session was created. Treat \
                             their content and properties as persistent user context; tool changes \
                             made later in this session do not alter this snapshot."
            .to_owned(),
    }
}

fn pinned_limits() -> PinnedMemoryLimits {
    PinnedMemoryLimits {
        max_entries: non_zero(64),
        max_id_bytes: non_zero(32),
        max_category_bytes: non_zero(64),
        max_content_bytes: non_zero(4096),
        max_attributes_per_entry: non_zero(16),
        max_attribute_key_bytes: non_zero(64),
        max_attribute_string_bytes: non_zero(512),
        max_description_bytes: non_zero(1024),
        max_snapshot_bytes: non_zero(256 * 1024),
    }
}

async fn initialize_recall_file(path: &Path) -> Result<(), DemoError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| DemoError::Io(error.to_string()))?
    {
        return Ok(());
    }
    let records = DemoRecallFile::new(vec![
        DemoRecallRecord {
            reference: "demo://project/architecture".to_owned(),
            content: "ez-assistant is a local-first desktop AI assistant whose Agent Core is \
                      independent from the application Runtime."
                .to_owned(),
            attributes: BTreeMap::from([
                (
                    "kind".to_owned(),
                    MemoryPropertyValue::String("project_note".to_owned()),
                ),
                (
                    "priority".to_owned(),
                    MemoryPropertyValue::Number(serde_json::Number::from(2)),
                ),
            ]),
        },
        DemoRecallRecord {
            reference: "demo://meeting/memory-design".to_owned(),
            content: "Pinned memory belongs in a frozen system prompt snapshot, while larger \
                      historical knowledge should be queried through recall_memory."
                .to_owned(),
            attributes: BTreeMap::from([(
                "kind".to_owned(),
                MemoryPropertyValue::String("meeting_note".to_owned()),
            )]),
        },
    ]);
    AtomicJsonWriter::default()
        .write(path, &records)
        .await
        .map_err(|error| DemoError::Io(error.to_string()))
}

fn non_zero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("demo configuration is non-zero")
}

fn tool_registration_error(error: agent_tools::RegisterToolError) -> DemoError {
    DemoError::ToolRegistration(error.to_string())
}

struct FailingDemoSource {
    id: RecallSourceId,
}

impl RecallSource for FailingDemoSource {
    fn id(&self) -> &RecallSourceId {
        &self.id
    }

    fn recall(
        &self,
        _request: RecallSourceRequest,
        cancellation: CancellationToken,
    ) -> RecallSourceFuture<'_, RecallSourceResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                Err(RecallSourceError::Cancelled)
            } else {
                Err(RecallSourceError::Unavailable {
                    message: "intentional demo source failure".to_owned(),
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use agent_memory::{MemoryRecallRequest, RecallFailureKind};

    use super::*;

    #[test]
    fn chat_uses_explicit_deepseek_thinking_request_config() {
        let config = deepseek_model_request_config();
        assert_eq!(config.reasoning, Some(ReasoningConfig { effort: None }));
        assert_eq!(
            config.provider_options.get("deepseek"),
            Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
        );
    }

    #[tokio::test]
    async fn initialization_creates_examples_once_and_registers_fixed_tools() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let resources = build_memory_resources(directory.path())
            .await
            .expect("build memory resources");
        assert_eq!(resources.tools.len(), 5);
        assert_eq!(
            resources
                .tools
                .definitions()
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "pin_memory",
                "update_pinned_memory",
                "unpin_memory",
                "list_pinned_memories",
                "recall_memory"
            ]
        );
        let path = directory.path().join(RECALL_FILE);
        let original = tokio::fs::read(&path).await.expect("read examples");
        tokio::fs::write(&path, b"custom-data")
            .await
            .expect("replace examples");
        drop(resources);
        assert!(build_memory_resources(directory.path()).await.is_ok());
        assert_eq!(
            tokio::fs::read(&path).await.expect("read custom data"),
            b"custom-data"
        );
        assert_ne!(original, b"custom-data");
    }

    #[tokio::test]
    async fn default_explicit_and_failing_sources_are_observable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        initialize_recall_file(&directory.path().join(RECALL_FILE))
            .await
            .expect("initialize recall file");
        let demo_id = RecallSourceId::new(DEMO_SOURCE_ID).expect("valid source id");
        let failing_id = RecallSourceId::new(FAILING_SOURCE_ID).expect("valid source id");
        let recall = CoordinatedMemoryRecall::new(
            vec![
                Arc::new(DemoRecallSource::new(
                    demo_id.clone(),
                    directory.path().join(RECALL_FILE),
                    non_zero(4096),
                )),
                Arc::new(FailingDemoSource {
                    id: failing_id.clone(),
                }),
            ],
            CoordinatedMemoryRecallConfig {
                default_sources: vec![demo_id.clone()],
                source_timeout: Duration::from_secs(1),
                max_sources: non_zero(2),
                max_query_bytes: non_zero(1024),
                max_source_id_bytes: non_zero(64),
                max_item_bytes: non_zero(4096),
            },
        )
        .expect("build coordinator");
        let request = |sources| MemoryRecallRequest {
            query: "memory".to_owned(),
            scope: agent_memory::RecallScope::Session,
            limit: non_zero(4),
            sources,
        };
        let default = recall
            .recall(request(None), CancellationToken::new())
            .await
            .expect("default recall");
        assert!(!default.items.is_empty());
        assert_eq!(default.items[0].origins[0].source_id, demo_id);

        let explicit = recall
            .recall(
                request(Some(vec![demo_id.clone()])),
                CancellationToken::new(),
            )
            .await
            .expect("explicit demo recall");
        assert!(!explicit.items.is_empty());
        assert!(explicit.failures.is_empty());

        let partial = recall
            .recall(
                request(Some(vec![demo_id.clone(), failing_id.clone()])),
                CancellationToken::new(),
            )
            .await
            .expect("partial recall");
        assert!(!partial.items.is_empty());
        assert_eq!(partial.failures[0].source_id, failing_id);
        assert_eq!(partial.failures[0].kind, RecallFailureKind::Unavailable);

        assert!(
            recall
                .recall(
                    request(Some(vec![
                        RecallSourceId::new(FAILING_SOURCE_ID).expect("valid source id")
                    ])),
                    CancellationToken::new(),
                )
                .await
                .is_err()
        );
    }
}
