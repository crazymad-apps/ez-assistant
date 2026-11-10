//! 只在真实 Run／子执行装配发布器；模型无法指定 owner、收据或替换页面代码地址。

use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use assistant_protocol::{
    ConversationOwner, MessageCardSource, PluginUiEntryKind, PluginUiResource, RunId, RuntimeEvent,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    NewMessageCard, RuntimeError, RuntimeResult, RuntimeStore, mcp::McpUiBinding,
    observation::ObservationCoordinator, plugin::PluginManager, session::SessionController,
};

#[derive(Clone)]
pub(crate) struct CardPublisher {
    pub(crate) session: Arc<SessionController>,
    owner: ConversationOwner,
    run_id: Option<RunId>,
    store: Arc<dyn RuntimeStore>,
    plugin: Arc<PluginManager>,
    events: ObservationCoordinator,
}

impl CardPublisher {
    pub(crate) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        store: Arc<dyn RuntimeStore>,
        plugin: Arc<PluginManager>,
        events: ObservationCoordinator,
    ) -> Self {
        Self {
            owner: ConversationOwner::MainSession {
                session_id: session.id().clone(),
            },
            session,
            run_id: Some(run_id),
            store,
            plugin,
            events,
        }
    }

    pub(crate) fn for_child(&self, id: assistant_protocol::ChildTaskId) -> Self {
        let mut publisher = self.clone();
        publisher.owner = ConversationOwner::ChildTask {
            session_id: self.session.id().clone(),
            child_task_id: id,
        };
        publisher.run_id = None;
        publisher
    }

    pub(crate) async fn validate_mcp(&self, binding: &McpUiBinding) -> RuntimeResult<()> {
        if let (assistant_protocol::McpServerSource::Plugin(name), Some(id)) =
            (&binding.target.source, &binding.entry_id)
        {
            let (entry, lease, _) = self.plugin.ui_entry(name, id).await?;
            if Some(&lease.sha256) != binding.plugin_sha256.as_ref()
                || (entry.kind == PluginUiEntryKind::Card) != binding.card
                || !matches!(&entry.resource,PluginUiResource::Mcp{server,uri} if server==binding.target.server_key.as_str() && uri==&binding.uri)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub(crate) async fn publish_mcp(
        &self,
        binding: McpUiBinding,
        arguments: Value,
        result: Value,
        context: &ToolContext,
    ) -> RuntimeResult<()> {
        self.validate_mcp(&binding).await?;
        self.commit(
            MessageCardSource::Mcp {
                target: binding.target,
                resource_uri: binding.uri,
                entry_id: binding.entry_id,
                published_plugin_sha256: binding.plugin_sha256,
            },
            binding.title,
            arguments,
            result,
            Some(binding.fingerprint),
            context,
        )
        .await
        .map(|_| ())
    }

    async fn commit(
        &self,
        source: MessageCardSource,
        title: String,
        arguments: Value,
        tool_result: Value,
        fingerprint: Option<String>,
        context: &ToolContext,
    ) -> RuntimeResult<String> {
        let call = context.call_id().ok_or_else(invalid)?;
        let card = NewMessageCard {
            card_id: crate::id::generate("card").map_err(|_| invalid())?,
            owner: self.owner.clone(),
            run_id: self.run_id.clone(),
            tool_call_id: assistant_protocol::ToolCallId::new(call.as_str())
                .map_err(|_| invalid())?,
            title,
            source,
            arguments,
            tool_result,
            source_fingerprint: fingerprint,
        };
        // 先持有 Session gate，再由 Store 读取已可靠 started 的收据；提交后才发出无载荷事件。
        // 不改变正文或执行历史；崩溃恢复依赖原工具交换，不重放业务副作用。
        let _mutation = self.session.mutation().await;
        if context.cancellation.is_cancelled() {
            return Err(invalid());
        }
        let saved = self
            .store
            .publish_message_card(card)
            .await
            .map_err(|e| RuntimeError::from_store("publish message card", e))?;
        let _ = self.events.send(RuntimeEvent::MessageCardsChanged {
            owner: self.owner.clone(),
        });
        Ok(saved.summary.card_id)
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublishCardInput {
    plugin: String,
    entry: String,
    arguments: std::collections::BTreeMap<String, Value>,
}

pub(crate) struct CardAuthorizationFacts;

pub(crate) struct PublishCardTool(pub(crate) CardPublisher);

impl Tool for PublishCardTool {
    type Input = PublishCardInput;
    type ResolvedInput = PublishCardInput;
    type Output = Value;

    fn name(&self) -> agent_types::ToolName {
        agent_types::ToolName::new("publish_card").expect("static tool name")
    }

    fn description(&self) -> String {
        "Publish one declared card from an installed, enabled plugin. Use its exact plugin name and card entry ID, with arguments matching the entry schema. This saves a card in the current conversation; it does not execute a backend tool. Do not pass HTML or a URL.".into()
    }

    fn resolve(
        &self,
        input: PublishCardInput,
    ) -> Result<ToolResolution<PublishCardInput>, ToolError> {
        if input.plugin.is_empty()
            || input.plugin.len() > 64
            || input.entry.is_empty()
            || input.entry.len() > 64
            || serde_json::to_vec(&input.arguments).map_or(true, |b| b.len() > 256 * 1024)
        {
            return Err(ToolError::invalid_input("card entry or arguments invalid"));
        }
        let semantic = json!({"plugin":input.plugin,"entry":input.entry});
        Ok(ToolResolution::with_facts(
            input,
            CardAuthorizationFacts,
            semantic,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: PublishCardInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Value> {
        Box::pin(async move {
            let publisher = &self.0;
            let (entry, lease, _) = publisher
                .plugin
                .ui_entry(&input.plugin, &input.entry)
                .await
                .map_err(|_| ToolError::execution("card entry unavailable"))?;
            if entry.kind != PluginUiEntryKind::Card {
                return Err(ToolError::invalid_input("entry is not a card"));
            }
            let arguments = Value::Object(input.arguments.into_iter().collect());
            let schema = entry.input_schema.clone();
            let checked = arguments.clone();
            let valid = tokio::task::spawn_blocking(move || {
                crate::mcp::compile_schema(&schema).is_ok_and(|s| s.is_valid(&checked))
            })
            .await
            .unwrap_or(false);
            if !valid {
                return Err(ToolError::invalid_input(
                    "card arguments do not match entry schema",
                ));
            }
            let source = MessageCardSource::Plugin {
                name: input.plugin,
                entry_id: input.entry,
                published_sha256: lease.sha256.clone(),
            };
            let id = publisher
                .commit(
                    source,
                    entry.title,
                    arguments,
                    json!({"content":[]}),
                    None,
                    &context,
                )
                .await
                .map_err(|_| ToolError::execution("card could not be saved"))?;
            Ok(json!({"card_id":id,"status":"published"}))
        })
    }
}

fn invalid() -> RuntimeError {
    RuntimeError::InvalidRequest {
        reason: "卡片入口或执行身份已失效",
    }
}
