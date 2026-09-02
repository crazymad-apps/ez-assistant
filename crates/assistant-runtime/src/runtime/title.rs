//! 自动标题与 `/title` 共用的独立模型调用链路。

use std::{sync::Arc, time::Duration};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ModelRequest, ProviderOptions,
    SystemPromptSnapshot, collect_model_turn,
};
use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart,
    ToolChoice, ToolDefinition, ToolName, TranscriptVisibility, UserMessage, UserMessageOrigin,
    UserPart,
};
use assistant_protocol::{
    GenerateSessionTitleRequest, GenerateSessionTitleResult, RuntimeEvent,
    SessionTitleGenerationFinishedOutcome, SessionTitleGenerationSnapshot,
    SessionTitleGenerationTriggerSnapshot,
};
use serde_json::json;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{AssistantRuntime, model::compile_model_service};
use crate::{
    ModelServiceFactory, RuntimeError, RuntimeResult, RuntimeStore, SessionTitleGenerationCommit,
    config::ConfigRegistry,
    observation::ObservationCoordinator,
    session::{ActiveSessionTitleGeneration, SessionController},
};

const TITLE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TITLE_CONTEXT_BYTES: usize = 12 * 1024;
const MAX_TITLE_CHARS: usize = 80;
const MAX_TITLE_OUTPUT_TOKENS: u32 = 1_024;
const TITLE_OUTPUT_TOOL: &str = "submit_session_title";
const TITLE_SYSTEM_PROMPT: &str = "你是会话标题生成器，不是当前对话的参与者。阅读上下文后，只调用一次 submit_session_title，不要继续回答用户，也不要输出普通文本。title 使用简洁、准确的名词性短语概括用户当前主要意图，不要包含解释、引号、Markdown 标记或句末标点，并避免复述日志、路径和无关上下文。";

#[derive(Clone)]
pub(super) struct TitleGenerationContext {
    config_registry: Arc<ConfigRegistry>,
    model_factory: Arc<dyn ModelServiceFactory>,
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
    root_cancellation: CancellationToken,
    tasks: Arc<super::tasks::RuntimeTasks>,
}

impl TitleGenerationContext {
    pub(super) fn new(
        config_registry: Arc<ConfigRegistry>,
        model_factory: Arc<dyn ModelServiceFactory>,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
        root_cancellation: CancellationToken,
        tasks: Arc<super::tasks::RuntimeTasks>,
    ) -> Self {
        Self {
            config_registry,
            model_factory,
            store,
            events,
            root_cancellation,
            tasks,
        }
    }
}

struct PreparedTitleGeneration {
    task_id: String,
    trigger: SessionTitleGenerationTriggerSnapshot,
    snapshot: SessionTitleGenerationSnapshot,
    expected_title: String,
    model: Arc<dyn agent_model::ModelService>,
    request: ModelRequest,
    cancellation: CancellationToken,
}

impl AssistantRuntime {
    pub(super) fn title_generation_context(&self) -> TitleGenerationContext {
        TitleGenerationContext::new(
            self.config_registry.clone(),
            self.model_factory.clone(),
            self.store.clone(),
            self.event_sender.clone(),
            self.root_cancellation.clone(),
            self.tasks.clone(),
        )
    }

    /// 手动触发只负责登记旁路任务；模型执行不占用 HTTP command future。
    pub async fn generate_session_title(
        &self,
        request: GenerateSessionTitleRequest,
    ) -> RuntimeResult<GenerateSessionTitleResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let context = self.title_generation_context();
        let prepared = begin_title_generation(
            &context,
            &session,
            SessionTitleGenerationTriggerSnapshot::Manual,
        )
        .await?
        .ok_or(RuntimeError::InvalidRequest {
            reason: "session conversation does not contain a completed user and assistant exchange",
        })?;
        let generation = prepared.snapshot.clone();
        spawn_title_generation(context, session, prepared);
        Ok(GenerateSessionTitleResult { generation })
    }
}

pub(super) fn schedule_automatic_title(
    context: TitleGenerationContext,
    session: Arc<SessionController>,
) {
    let task_context = context.clone();
    let panic_session = session.clone();
    context.tasks.spawn(
        async move {
            if session
                .ensure_conversation_loaded(task_context.store.as_ref())
                .await
                .is_err()
            {
                return;
            }
            if let Ok(Some(prepared)) = begin_title_generation(
                &task_context,
                &session,
                SessionTitleGenerationTriggerSnapshot::Automatic,
            )
            .await
            {
                execute_title_generation(task_context, session, prepared).await;
            }
        },
        recover_panicked_title_generation(panic_session),
    );
}

fn spawn_title_generation(
    context: TitleGenerationContext,
    session: Arc<SessionController>,
    prepared: PreparedTitleGeneration,
) {
    let panic_session = session.clone();
    let tasks = context.tasks.clone();
    tasks.spawn(
        execute_title_generation(context, session, prepared),
        recover_panicked_title_generation(panic_session),
    );
}

async fn recover_panicked_title_generation(session: Arc<SessionController>) {
    if let Ok(mut state) = session.lock_state()
        && let Some(active) = state.active_title_generation.take()
    {
        active.cancellation.cancel();
    }
}

async fn begin_title_generation(
    context: &TitleGenerationContext,
    session: &Arc<SessionController>,
    trigger: SessionTitleGenerationTriggerSnapshot,
) -> RuntimeResult<Option<PreparedTitleGeneration>> {
    let _mutation = session.mutation().await;
    session.ensure_healthy()?;
    session.ensure_active()?;
    session.ensure_standard_role()?;

    let (conversation, model_key, expected_title, pending) = {
        let state = session.lock_state()?;
        let conversation = state
            .journal
            .as_ref()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "title generation conversation",
            })?
            .snapshot();
        (
            conversation,
            state.model_key.clone(),
            state.title.clone(),
            state.automatic_title_pending,
        )
    };
    if trigger == SessionTitleGenerationTriggerSnapshot::Automatic && !pending {
        return Ok(None);
    }
    let Some(transcript) = select_title_context(&conversation) else {
        return Ok(None);
    };
    if trigger == SessionTitleGenerationTriggerSnapshot::Manual {
        context
            .store
            .disable_automatic_title(session.id())
            .await
            .map_err(|source| RuntimeError::from_store("disable automatic title", source))?;
        session.lock_state()?.automatic_title_pending = false;
    }
    let config = context.config_registry.snapshot()?;
    let compiled = compile_model_service(&config, &model_key, context.model_factory.as_ref())?;
    if !compiled.model.capabilities().tool_calls {
        return Err(RuntimeError::InvalidRequest {
            reason: "session model does not support the title output contract",
        });
    }
    let max_output_tokens = compiled.max_output_tokens.min(MAX_TITLE_OUTPUT_TOKENS);
    let tool_choice = title_tool_choice(compiled.model.capabilities());
    let cancellation = context.root_cancellation.child_token();
    let started_at_ms = super::now_ms()?;
    let snapshot = SessionTitleGenerationSnapshot {
        trigger,
        started_at_ms,
    };
    let task_id =
        crate::id::generate("title").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "title generation id random source",
        })?;
    {
        let mut state = session.lock_state()?;
        if let Some(previous) = state.active_title_generation.take() {
            previous.cancellation.cancel();
        }
        state.active_title_generation = Some(ActiveSessionTitleGeneration {
            task_id: task_id.clone(),
            snapshot: snapshot.clone(),
            cancellation: cancellation.clone(),
        });
    }
    let _ = context
        .events
        .send(RuntimeEvent::SessionTitleGenerationStarted {
            session_id: session.id().clone(),
            generation: snapshot.clone(),
        });
    Ok(Some(PreparedTitleGeneration {
        task_id,
        trigger,
        snapshot,
        expected_title,
        model: compiled.model,
        request: title_request(transcript, max_output_tokens, tool_choice),
        cancellation,
    }))
}

async fn execute_title_generation(
    context: TitleGenerationContext,
    session: Arc<SessionController>,
    prepared: PreparedTitleGeneration,
) {
    let call = collect_model_turn(
        prepared.model.as_ref(),
        prepared.request,
        ModelCallContext::new(prepared.cancellation.clone()),
    );
    let response = match timeout(TITLE_TIMEOUT, call).await {
        Ok(result) => result.ok(),
        Err(_) => {
            prepared.cancellation.cancel();
            None
        }
    };
    let usage = response.as_ref().and_then(|message| message.usage.clone());
    let candidate = response.as_ref().and_then(title_from_tool_call);
    let _mutation = session.mutation().await;
    let is_current = session
        .lock_state()
        .ok()
        .and_then(|state| {
            state
                .active_title_generation
                .as_ref()
                .map(|active| active.task_id == prepared.task_id)
        })
        .unwrap_or(false);
    if !is_current {
        return;
    }
    let committed = context
        .store
        .commit_session_title_generation(SessionTitleGenerationCommit {
            session_id: session.id().clone(),
            trigger: prepared.trigger,
            expected_title: (prepared.trigger == SessionTitleGenerationTriggerSnapshot::Automatic)
                .then_some(prepared.expected_title),
            title: candidate,
            request_attempted: true,
            usage,
            completed_at_ms: super::now_ms().unwrap_or(prepared.snapshot.started_at_ms),
        })
        .await;
    let outcome = match committed {
        Ok(result) => {
            if let Ok(mut state) = session.lock_state() {
                state.active_title_generation = None;
                state.automatic_title_pending = result.automatic_title_pending;
                state.title = result.title;
                state.title_origin = result.title_origin;
            }
            if result.applied {
                let _ = context.events.send(RuntimeEvent::SessionChanged {
                    session_id: session.id().clone(),
                });
                SessionTitleGenerationFinishedOutcome::Succeeded
            } else {
                SessionTitleGenerationFinishedOutcome::Failed
            }
        }
        Err(_) => {
            if let Ok(mut state) = session.lock_state() {
                state.active_title_generation = None;
            }
            SessionTitleGenerationFinishedOutcome::Failed
        }
    };
    let _ = context
        .events
        .send(RuntimeEvent::SessionTitleGenerationFinished {
            session_id: session.id().clone(),
            trigger: prepared.trigger,
            outcome,
        });
}

fn title_request(
    transcript: String,
    max_output_tokens: u32,
    tool_choice: ToolChoice,
) -> ModelRequest {
    let user = UserMessage {
        id: MessageId::new("title-context").expect("static message id is valid"),
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("title-context-text").expect("static part id is valid"),
            text: transcript,
        })],
    };
    ModelRequest {
        system: SystemPromptSnapshot::new(vec![TITLE_SYSTEM_PROMPT.to_owned()]),
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(user)]),
        tools: vec![title_output_tool()],
        tool_choice,
        generation: GenerationConfig {
            // `reasoning: None` means the side path does not request a reasoning mode, but some
            // reasoning-first models still apply their provider default. Keep enough bounded room
            // for that prefix so the concise title text is not cut off before it starts.
            max_output_tokens: Some(max_output_tokens),
            ..GenerationConfig::default()
        },
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn title_output_tool() -> ToolDefinition {
    ToolDefinition {
        name: title_tool_name(),
        description: "提交当前对话的最终标题。只调用一次；title 必须是概括用户主要意图的简短名词性短语，不能续写或回答对话。"
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "title": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TITLE_CHARS
                }
            },
            "required": ["title"]
        }),
    }
}

fn title_tool_name() -> ToolName {
    ToolName::new(TITLE_OUTPUT_TOOL).expect("static title tool name is valid")
}

fn title_tool_choice(capabilities: &ModelCapabilities) -> ToolChoice {
    if capabilities.tool_choice.named {
        ToolChoice::Named(title_tool_name())
    } else if capabilities.tool_choice.required {
        ToolChoice::Required
    } else {
        // Auto is the only explicit capability guaranteed by the catalog for tool-capable
        // OpenAI-compatible routes such as DeepSeek thinking. The fixed prompt and single tool
        // still request the call; validation below rejects any plain-text fallback.
        ToolChoice::Auto
    }
}

fn select_title_context(conversation: &ConversationSnapshot) -> Option<String> {
    let summary = conversation
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ConversationMessage::ContextSummary(summary) if !summary.text.trim().is_empty() => {
                Some(format!("上下文摘要：{}", summary.text.trim()))
            }
            _ => None,
        });
    let mut entries = Vec::new();
    let mut has_user = false;
    let mut has_assistant = false;
    for message in &conversation.messages {
        match message {
            ConversationMessage::User(user) if user.transcript_visibility.is_visible() => {
                let mut parts = Vec::new();
                for part in &user.parts {
                    match part {
                        UserPart::Text(text) if !text.text.trim().is_empty() => {
                            parts.push(text.text.trim().to_owned());
                        }
                        UserPart::QuotedText(quote) if !quote.exact.trim().is_empty() => {
                            parts.push(format!("引用：{}", quote.exact.trim()));
                        }
                        UserPart::FileReferences(files) if !files.files.is_empty() => {
                            parts.push(format!(
                                "附件：{}",
                                files
                                    .files
                                    .iter()
                                    .map(|file| file.original_name.as_str())
                                    .collect::<Vec<_>>()
                                    .join("、")
                            ));
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    has_user = true;
                    entries.push(format!("用户：{}", parts.join("\n")));
                }
            }
            ConversationMessage::Assistant(assistant) => {
                let text = assistant
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text(text) if !text.text.trim().is_empty() => {
                            Some(text.text.trim())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    has_assistant = true;
                    entries.push(format!("助手：{text}"));
                }
            }
            _ => {}
        }
    }
    if !has_user || !has_assistant {
        return None;
    }
    let first_user = entries
        .iter()
        .position(|entry| entry.starts_with("用户："))?;
    let mut fixed = vec![entries[first_user].clone()];
    if let Some(summary) = summary
        && fixed[0].len() + 2 + summary.len() <= MAX_TITLE_CONTEXT_BYTES
    {
        fixed.push(summary);
    }
    let fixed_bytes =
        fixed.iter().map(String::len).sum::<usize>() + fixed.len().saturating_sub(1) * 2;
    let mut recent = Vec::new();
    let mut remaining = MAX_TITLE_CONTEXT_BYTES.saturating_sub(fixed_bytes + 2);
    for (index, entry) in entries.iter().enumerate().rev() {
        if index == first_user {
            continue;
        }
        let required = entry.len() + usize::from(!recent.is_empty()) * 2;
        if required <= remaining {
            recent.push(entry.clone());
            remaining -= required;
        }
    }
    recent.reverse();
    fixed.extend(recent);
    let mut output = String::new();
    for entry in fixed {
        let separator = if output.is_empty() { "" } else { "\n\n" };
        let remaining = MAX_TITLE_CONTEXT_BYTES.saturating_sub(output.len() + separator.len());
        if remaining == 0 {
            break;
        }
        output.push_str(separator);
        output.push_str(&truncate_utf8(&entry, remaining));
    }
    Some(output)
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn title_from_tool_call(message: &agent_types::AssistantMessage) -> Option<String> {
    if message
        .parts
        .iter()
        .any(|part| matches!(part, AssistantPart::Text(_)))
    {
        return None;
    }
    let mut calls = message.parts.iter().filter_map(|part| match part {
        AssistantPart::ToolCall(call) => Some(call),
        _ => None,
    });
    let call = calls.next()?;
    if calls.next().is_some() || call.name != title_tool_name() {
        return None;
    }
    let arguments = call.arguments.as_object()?;
    if arguments.len() != 1 {
        return None;
    }
    let title = arguments.get("title")?.as_str()?;
    clean_title_value(title)
}

fn clean_title_value(value: &str) -> Option<String> {
    let first_line = value.lines().find(|line| !line.trim().is_empty())?;
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let without_heading = collapsed.trim_start_matches('#').trim();
    let unquoted = strip_paired_quotes(without_heading).trim();
    let title = unquoted.chars().take(MAX_TITLE_CHARS).collect::<String>();
    (!title.is_empty()).then_some(title)
}

fn strip_paired_quotes(value: &str) -> &str {
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('“', '”'),
        ('‘', '’'),
        ('《', '》'),
    ] {
        if value.starts_with(open) && value.ends_with(close) && value.chars().count() >= 2 {
            let start = open.len_utf8();
            let end = value.len() - close.len_utf8();
            return &value[start..end];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_types::{FinishReason, ModelIdentity, ProviderId, ToolCall, ToolCallId};

    #[test]
    fn title_tool_call_removes_markdown_quotes_and_limits_scalars() {
        let message = agent_types::AssistantMessage {
            id: MessageId::new("assistant-title").expect("valid id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("valid provider"),
                "fixture",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("assistant-title-call").expect("valid id"),
                name: title_tool_name(),
                arguments: json!({"title": format!("## “{}”\nignored", "标题".repeat(50))}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        let title = title_from_tool_call(&message).expect("valid title");
        assert!(!title.starts_with('#'));
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
    }

    #[test]
    fn title_output_rejects_plain_text_and_ambiguous_calls() {
        let plain_text = agent_types::AssistantMessage {
            id: MessageId::new("assistant-continuation").expect("valid id"),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("valid provider"),
                "fixture",
            ),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new("assistant-continuation-text").expect("valid id"),
                text: "调研完成，更新计划如下。".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        assert_eq!(title_from_tool_call(&plain_text), None);

        let duplicate_calls = agent_types::AssistantMessage {
            id: MessageId::new("assistant-duplicate-title").expect("valid id"),
            model: plain_text.model,
            parts: ["first-title-call", "second-title-call"]
                .into_iter()
                .map(|id| {
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new(id).expect("valid id"),
                        name: title_tool_name(),
                        arguments: json!({"title": "标题"}),
                    })
                })
                .collect(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        assert_eq!(title_from_tool_call(&duplicate_calls), None);
    }

    #[test]
    fn title_tool_choice_uses_the_strongest_supported_contract() {
        let mut capabilities = ModelCapabilities {
            tool_calls: true,
            tool_choice: agent_model::ToolChoiceCapabilities::auto_only(),
            ..ModelCapabilities::default()
        };
        assert_eq!(title_tool_choice(&capabilities), ToolChoice::Auto);

        capabilities.tool_choice.required = true;
        assert_eq!(title_tool_choice(&capabilities), ToolChoice::Required);

        capabilities.tool_choice.named = true;
        assert_eq!(
            title_tool_choice(&capabilities),
            ToolChoice::Named(title_tool_name())
        );
    }
}
