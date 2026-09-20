//! 同模型 Rolling Summary 的配置与策略实现。

use agent_model::{ModelCallContext, ModelError, collect_model_turn};
use agent_types::{
    AssistantMessage, AssistantPart, ContextInsertionPayload, ContextInsertionPlan,
    ContextSummaryMessage, ContextUsageAdjustment, ConversationMessage, ConversationSnapshot,
    InternalContextPart, MessageId, PartId, ToolChoice, TranscriptVisibility, UserMessage,
    UserMessageOrigin, UserPart,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    CompactionCandidate, CompactionError, CompactionFuture, CompactionInput, CompressionStrategy,
    ReplacementEffectError, StrategyOutcome, StrategyReport, validate_replacement,
    validate_replacement_effect,
};

/// 策略报告中的稳定名称。
const STRATEGY_NAME: &str = "rolling_summary_same_model";

/// 构造 compression request 时追加到可压缩 head 末尾的临时指令。
const COMPRESSION_INSTRUCTIONS: &str = "\
Summarize the earlier conversation for use as context in future turns. Preserve user intent, \
decisions, constraints, unresolved questions, and important tool results. Do not call tools. \
Return only the concise summary.";

/// 临时压缩消息只存在于单次请求中，不写入历史或 replacement。
const COMPRESSION_MESSAGE_ID: &str = "context_compaction_instruction";
const COMPRESSION_PART_ID: &str = "context_compaction_instruction_text";
/// 同模型滚动摘要策略配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollingSummaryPolicy {
    summary_output_tokens: u32,
    minimum_recent_user_turns: u32,
}

impl RollingSummaryPolicy {
    /// 创建构造期已验证的滚动摘要配置。
    pub fn new(
        summary_output_tokens: u32,
        minimum_recent_user_turns: u32,
    ) -> Result<Self, RollingSummaryPolicyError> {
        if summary_output_tokens == 0 {
            return Err(RollingSummaryPolicyError::ZeroSummaryOutputTokens);
        }
        Ok(Self {
            summary_output_tokens,
            minimum_recent_user_turns,
        })
    }

    /// 压缩请求允许生成的最大输出 token。
    pub fn summary_output_tokens(&self) -> u32 {
        self.summary_output_tokens
    }

    /// replacement 中至少原样保留的近期 User Turn 数。
    pub fn minimum_recent_user_turns(&self) -> u32 {
        self.minimum_recent_user_turns
    }
}

/// 使用当前执行配置中的同一个 [`agent_model::ModelService`] 生成滚动摘要。
///
/// 本策略只发起一次无工具 Model Turn 并生成候选 replacement；不提交 Checkpoint，
/// 不驱动 Agent Loop，也不决定是否 continuation。
#[derive(Clone, Debug)]
pub struct RollingSummarySameModel {
    policy: RollingSummaryPolicy,
}

impl RollingSummarySameModel {
    /// 使用构造期已验证的策略配置创建实例。
    pub fn new(policy: RollingSummaryPolicy) -> Self {
        Self { policy }
    }

    /// 返回当前策略配置。
    pub fn policy(&self) -> &RollingSummaryPolicy {
        &self.policy
    }
}

impl CompressionStrategy for RollingSummarySameModel {
    fn compact<'a>(
        &'a self,
        input: CompactionInput,
        cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        let policy = self.policy.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(CompactionError::Cancelled);
            }

            let CompactionInput {
                model,
                normal_request,
                layout,
            } = input;
            let partition = layout.partition(policy.minimum_recent_user_turns());
            let compacted_usage = aggregate_replaced_usage(&partition);
            let compressed_blocks = report_count(partition.compressible_head().len())?;
            let retained_blocks = report_count(partition.protected_tail().len())?;
            if !partition.has_compressible_head() {
                return Ok(StrategyOutcome::NoOp {
                    report: StrategyReport {
                        strategy: STRATEGY_NAME.to_owned(),
                        compressed_blocks,
                        retained_blocks,
                        model: None,
                        usage: None,
                    },
                });
            }

            if !normal_request.tools.is_empty() && !model.capabilities().tool_choice.none {
                return Err(CompactionError::UnsupportedToolSuppression);
            }
            let mut request = normal_request;
            request
                .conversation
                .messages
                .push(compression_instruction_message());
            request.tool_choice = ToolChoice::None;
            request.generation.max_output_tokens = Some(policy.summary_output_tokens());
            let message = collect_model_turn(
                model.as_ref(),
                request,
                ModelCallContext::new(cancellation.clone()),
            )
            .await
            .map_err(map_model_error)?;
            if cancellation.is_cancelled() {
                return Err(CompactionError::Cancelled);
            }

            let summary_text = summary_text(&message)?;
            let replacement = build_replacement(
                partition.protected_prefix(),
                partition.compressible_head(),
                partition.protected_tail(),
                &message,
                summary_text,
                compacted_usage,
            );
            validate_replacement(&replacement)?;
            validate_replacement_effect(&layout_snapshot(&layout), &replacement)
                .map_err(map_replacement_effect_error)?;

            Ok(StrategyOutcome::Candidate(CompactionCandidate {
                replacement,
                report: StrategyReport {
                    strategy: STRATEGY_NAME.to_owned(),
                    compressed_blocks,
                    retained_blocks,
                    model: Some(message.model),
                    usage: message.usage,
                },
            }))
        })
    }
}

fn map_replacement_effect_error(error: ReplacementEffectError) -> CompactionError {
    match error {
        ReplacementEffectError::Serialization => CompactionError::InvalidResponse {
            message: "context snapshot could not be serialized for size validation".to_owned(),
        },
        ReplacementEffectError::Ineffective {
            source_bytes,
            replacement_bytes,
        } => CompactionError::Ineffective {
            source_bytes,
            replacement_bytes,
        },
    }
}

fn compression_instruction_message() -> ConversationMessage {
    let part = InternalContextPart::new(
        PartId::new(COMPRESSION_PART_ID).expect("static compression part id must be valid"),
        "context_compaction_request",
        "context_compaction_instruction",
        COMPRESSION_INSTRUCTIONS,
    )
    .expect("static compression context must be valid");
    let plan = ContextInsertionPlan::request_only_internal("context_compaction_request", part);
    let ContextInsertionPayload::InternalContext(part) = plan.payload else {
        unreachable!("request-only internal plan carries internal context")
    };
    ConversationMessage::User(UserMessage {
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        id: MessageId::new(COMPRESSION_MESSAGE_ID)
            .expect("static compression message id must be valid"),
        parts: vec![UserPart::InternalContext(part)],
    })
}

fn report_count(count: usize) -> Result<u32, CompactionError> {
    u32::try_from(count).map_err(|_| CompactionError::InvalidResponse {
        message: "context layout contains too many blocks to report".to_owned(),
    })
}

fn map_model_error(error: ModelError) -> CompactionError {
    match error {
        ModelError::Cancelled => CompactionError::Cancelled,
        other => CompactionError::Model(other),
    }
}

fn summary_text(message: &AssistantMessage) -> Result<String, CompactionError> {
    if message
        .parts
        .iter()
        .any(|part| matches!(part, AssistantPart::ToolCall(_)))
    {
        return Err(CompactionError::InvalidResponse {
            message: "compression response contained a tool call".to_owned(),
        });
    }
    let text = message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(part) if !part.text.trim().is_empty() => Some(part.text.trim()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Err(CompactionError::InvalidResponse {
            message: "compression response contained no non-empty text".to_owned(),
        });
    }
    Ok(text)
}

fn build_replacement(
    protected_prefix: &[ConversationMessage],
    compressible_head: &[crate::ContextBlock],
    protected_tail: &[crate::ContextBlock],
    message: &AssistantMessage,
    summary_text: String,
    compacted_usage: Option<agent_types::TokenUsage>,
) -> ConversationSnapshot {
    let usage_adjustment = usage_adjustment(compressible_head, protected_tail);
    let mut messages = protected_prefix.to_vec();
    messages.push(ConversationMessage::ContextSummary(ContextSummaryMessage {
        id: message.id.clone(),
        text: summary_text,
        model: Some(message.model.clone()),
        usage: message.usage.clone(),
        compacted_usage,
        usage_adjustment,
        programmatic_context: None,
    }));
    messages.extend(
        protected_tail
            .iter()
            .flat_map(|block| block.messages().iter().cloned()),
    );
    ConversationSnapshot::new(messages)
}

fn usage_adjustment(
    compressible_head: &[crate::ContextBlock],
    protected_tail: &[crate::ContextBlock],
) -> Option<ContextUsageAdjustment> {
    let retained_assistant = protected_tail
        .iter()
        .flat_map(|block| block.messages())
        .rev()
        .find_map(|message| match message {
            ConversationMessage::Assistant(message) if message.usage.is_some() => Some(message),
            _ => None,
        });
    let Some(retained_assistant) = retained_assistant else {
        // replacement 中没有旧 Assistant usage 时无需做减法；保持 None 可让首个新响应
        // 自然成为新的权威基数，同时压缩完成后的首次预检仍因无 Assistant 而 unavailable。
        return None;
    };

    let subtract_total_tokens = compressible_head
        .iter()
        .flat_map(|block| block.messages())
        .rev()
        .find_map(|message| match message {
            ConversationMessage::Assistant(message) => {
                message.usage.as_ref().map(|usage| usage.total_tokens)
            }
            _ => None,
        });
    if let Some(subtract_total_tokens) = subtract_total_tokens {
        return Some(ContextUsageAdjustment::Subtract {
            retained_assistant_id: retained_assistant.id.clone(),
            subtract_total_tokens,
        });
    }

    if let [block] = compressible_head
        && let [ConversationMessage::ContextSummary(summary)] = block.messages()
        && let Some(ContextUsageAdjustment::Subtract {
            retained_assistant_id,
            subtract_total_tokens,
        }) = &summary.usage_adjustment
        && retained_assistant_id == &retained_assistant.id
    {
        return Some(ContextUsageAdjustment::Subtract {
            retained_assistant_id: retained_assistant_id.clone(),
            subtract_total_tokens: *subtract_total_tokens,
        });
    }

    Some(ContextUsageAdjustment::Unavailable)
}

fn layout_snapshot(layout: &crate::ContextLayout) -> ConversationSnapshot {
    let mut messages = layout.protected_prefix().to_vec();
    messages.extend(
        layout
            .blocks()
            .iter()
            .flat_map(|block| block.messages().iter().cloned()),
    );
    ConversationSnapshot::new(messages)
}

fn aggregate_replaced_usage(
    partition: &crate::ContextPartition<'_>,
) -> Option<agent_types::TokenUsage> {
    let mut total: Option<agent_types::TokenUsage> = None;
    for message in partition
        .compressible_head()
        .iter()
        .flat_map(|block| block.messages())
    {
        match message {
            ConversationMessage::Assistant(message) => {
                add_usage(&mut total, message.usage.as_ref());
            }
            ConversationMessage::ContextSummary(message) => {
                add_usage(&mut total, message.compacted_usage.as_ref());
                add_usage(&mut total, message.usage.as_ref());
            }
            ConversationMessage::System(_)
            | ConversationMessage::User(_)
            | ConversationMessage::Tool(_) => {}
        }
    }
    total
}

fn add_usage(total: &mut Option<agent_types::TokenUsage>, usage: Option<&agent_types::TokenUsage>) {
    let Some(usage) = usage else {
        return;
    };
    let Some(target) = total.as_mut() else {
        *total = Some(usage.clone());
        return;
    };
    target.input_tokens = target.input_tokens.saturating_add(usage.input_tokens);
    target.output_tokens = target.output_tokens.saturating_add(usage.output_tokens);
    target.total_tokens = target.total_tokens.saturating_add(usage.total_tokens);
    target.cached_input_tokens = target
        .cached_input_tokens
        .zip(usage.cached_input_tokens)
        .map(|(current, value)| current.saturating_add(value));
    if let Some(value) = usage.reasoning_tokens {
        target.reasoning_tokens = Some(
            target
                .reasoning_tokens
                .unwrap_or_default()
                .saturating_add(value),
        );
    }
}

/// Rolling Summary 配置不满足构造约束。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RollingSummaryPolicyError {
    /// 摘要输出上限必须大于零。
    #[error("summary output tokens must be greater than zero")]
    ZeroSummaryOutputTokens,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use agent_model::{
        ModelCapabilities, ModelEvent, ModelEventStream, ModelRequest, ModelService,
        ModelStreamFuture, ReasoningConfig, ReasoningEffort, ToolChoiceCapabilities,
    };
    use agent_types::{
        AssistantPart, FinishReason, MessageId, ModelIdentity, OpaqueProviderState, PartId,
        ProtocolId, ProviderId, ReasoningPart, SystemMessage, TextPart, TokenUsage, ToolCall,
        ToolCallId, ToolDefinition, ToolMessage, ToolName, ToolResult, ToolResultContent,
        ToolResultStatus, UserMessage,
    };

    use super::*;

    enum Script {
        Events(Vec<ModelEvent>),
        EstablishmentError(ModelError),
    }

    struct ScriptedModel {
        capabilities: ModelCapabilities,
        scripts: Mutex<VecDeque<Script>>,
        requests: Mutex<Vec<ModelRequest>>,
    }

    impl ScriptedModel {
        fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
            Self {
                capabilities: ModelCapabilities::default(),
                scripts: Mutex::new(scripts.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn take_requests(&self) -> Vec<ModelRequest> {
            std::mem::take(&mut self.requests.lock().expect("requests lock"))
        }

        fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
            self.capabilities = capabilities;
            self
        }
    }

    impl ModelService for ScriptedModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            128_000
        }

        fn stream(
            &self,
            request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            self.requests.lock().expect("requests lock").push(request);
            let script = self.scripts.lock().expect("scripts lock").pop_front();
            Box::pin(async move {
                if context.cancellation.is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                match script {
                    Some(Script::Events(events)) => {
                        Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
                    }
                    Some(Script::EstablishmentError(error)) => Err(error),
                    None => Err(ModelError::Config(
                        "scripted compaction model received an unexpected request".to_owned(),
                    )),
                }
            })
        }
    }

    fn id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message id")
    }

    fn part_id(value: impl Into<String>) -> PartId {
        PartId::new(value).expect("valid part id")
    }

    fn model_identity() -> ModelIdentity {
        ModelIdentity::new(
            ProviderId::new("test").expect("valid provider id"),
            "summary-model",
        )
    }

    fn usage(total_tokens: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: total_tokens.saturating_sub(10),
            output_tokens: 10,
            total_tokens,
            cached_input_tokens: Some(4),
            reasoning_tokens: Some(2),
        }
    }

    fn user(value: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: id(value),
            parts: vec![],
        })
    }

    fn assistant(value: &str, usage: Option<TokenUsage>) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: id(value),
            model: model_identity(),
            parts: vec![AssistantPart::Text(TextPart {
                id: part_id(format!("{value}_text")),
                text: format!("answer from {value}"),
            })],
            finish_reason: FinishReason::Stop,
            usage,
        })
    }

    fn summary_message(
        id_value: &str,
        parts: Vec<AssistantPart>,
        finish_reason: FinishReason,
    ) -> AssistantMessage {
        AssistantMessage {
            id: id(id_value),
            model: model_identity(),
            parts,
            finish_reason,
            usage: Some(usage(42)),
        }
    }

    #[test]
    fn compacted_cache_usage_remains_unknown_when_any_request_omits_it() {
        let mut total = None;
        let known = usage(40);
        let mut unknown = usage(60);
        unknown.cached_input_tokens = None;

        add_usage(&mut total, Some(&known));
        add_usage(&mut total, Some(&unknown));

        let total = total.expect("usage total");
        assert_eq!(total.input_tokens, 80);
        assert_eq!(total.cached_input_tokens, None);
    }

    #[test]
    fn rolling_only_the_previous_summary_inherits_its_subtraction_boundary() {
        let snapshot = ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_old"),
                text: "old".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: Some(ContextUsageAdjustment::Subtract {
                    retained_assistant_id: id("assistant_1"),
                    subtract_total_tokens: 40,
                }),
                programmatic_context: None,
            }),
            user("user_1"),
            assistant("assistant_1", Some(usage(80))),
        ]);
        let layout = crate::ContextLayout::build(&snapshot).expect("layout");
        let partition = layout.partition(1);

        assert_eq!(
            usage_adjustment(partition.compressible_head(), partition.protected_tail()),
            Some(ContextUsageAdjustment::Subtract {
                retained_assistant_id: id("assistant_1"),
                subtract_total_tokens: 40,
            })
        );
    }

    fn message_events(message: &AssistantMessage) -> Vec<ModelEvent> {
        let mut events = vec![ModelEvent::TurnStarted {
            message_id: message.id.clone(),
            model: message.model.clone(),
        }];
        for part in &message.parts {
            match part {
                AssistantPart::Reasoning(part) => {
                    events.push(ModelEvent::ReasoningStarted {
                        id: part.id.clone(),
                    });
                    events.push(ModelEvent::ReasoningDelta {
                        id: part.id.clone(),
                        delta: part.text.clone(),
                    });
                    events.push(ModelEvent::ReasoningFinished {
                        id: part.id.clone(),
                    });
                }
                AssistantPart::Text(part) => {
                    events.push(ModelEvent::TextStarted {
                        id: part.id.clone(),
                    });
                    events.push(ModelEvent::TextDelta {
                        id: part.id.clone(),
                        delta: part.text.clone(),
                    });
                    events.push(ModelEvent::TextFinished {
                        id: part.id.clone(),
                    });
                }
                AssistantPart::ToolCall(call) => {
                    events.push(ModelEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    });
                    events.push(ModelEvent::ToolCallDelta {
                        id: call.id.clone(),
                        arguments_delta: call.arguments.to_string(),
                    });
                    events.push(ModelEvent::ToolCallFinished {
                        id: call.id.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
                AssistantPart::ProviderState(_) => {}
            }
        }
        if let Some(usage) = &message.usage {
            events.push(ModelEvent::UsageUpdated {
                usage: usage.clone(),
            });
        }
        events.push(ModelEvent::TurnFinished {
            message: message.clone(),
        });
        events
    }

    fn input(model: Arc<ScriptedModel>, snapshot: &ConversationSnapshot) -> CompactionInput {
        CompactionInput {
            model,
            normal_request: ModelRequest {
                system: agent_model::SystemPromptSnapshot::new(vec![
                    "normal agent instruction".to_owned(),
                    "stable prefix".to_owned(),
                ]),
                conversation: snapshot.clone(),
                tools: vec![],
                tool_choice: ToolChoice::None,
                generation: Default::default(),
                reasoning: None,
                provider_options: Default::default(),
            },
            layout: crate::ContextLayout::build(snapshot).expect("valid layout"),
        }
    }

    #[test]
    fn policy_rejects_zero_output_and_preserves_recent_turn_setting() {
        assert_eq!(
            RollingSummaryPolicy::new(0, 1),
            Err(RollingSummaryPolicyError::ZeroSummaryOutputTokens)
        );
        let policy = RollingSummaryPolicy::new(512, 2).expect("valid policy");
        assert_eq!(policy.summary_output_tokens(), 512);
        assert_eq!(policy.minimum_recent_user_turns(), 2);
    }

    #[tokio::test]
    async fn first_summary_builds_request_candidate_and_report_without_mutating_history() {
        let snapshot = ConversationSnapshot::new(vec![
            ConversationMessage::System(SystemMessage {
                id: id("system_1"),
                text: "original system".to_owned(),
            }),
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
            user("user_3"),
            assistant("assistant_3", Some(usage(40))),
        ]);
        let summary = summary_message(
            "summary_1",
            vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: part_id("reasoning_1"),
                    text: "private reasoning".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: part_id("summary_text_1"),
                    text: " condensed facts ".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: part_id("summary_text_2"),
                    text: "open question".to_owned(),
                }),
            ],
            FinishReason::Stop,
        );
        let model = Arc::new(ScriptedModel::new([Script::Events(message_events(
            &summary,
        ))]));
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(256, 1).expect("valid policy"));

        let outcome = strategy
            .compact(input(model.clone(), &snapshot), CancellationToken::new())
            .await
            .expect("compaction succeeds");
        let StrategyOutcome::Candidate(candidate) = outcome else {
            panic!("expected candidate");
        };

        assert_eq!(
            candidate.replacement.messages,
            vec![
                snapshot.messages[0].clone(),
                ConversationMessage::ContextSummary(ContextSummaryMessage {
                    id: id("summary_1"),
                    text: "condensed facts\nopen question".to_owned(),
                    model: Some(summary.model.clone()),
                    usage: summary.usage.clone(),
                    compacted_usage: Some(agent_types::TokenUsage {
                        input_tokens: 30,
                        output_tokens: 20,
                        total_tokens: 50,
                        cached_input_tokens: Some(8),
                        reasoning_tokens: Some(4),
                    }),
                    usage_adjustment: Some(ContextUsageAdjustment::Subtract {
                        retained_assistant_id: id("assistant_3"),
                        subtract_total_tokens: 30,
                    }),
                    programmatic_context: None,
                }),
                snapshot.messages[5].clone(),
                snapshot.messages[6].clone(),
            ]
        );
        assert_eq!(
            candidate.report,
            StrategyReport {
                strategy: STRATEGY_NAME.to_owned(),
                compressed_blocks: 2,
                retained_blocks: 1,
                model: Some(model_identity()),
                usage: Some(usage(42)),
            }
        );
        validate_replacement(&candidate.replacement).expect("candidate is valid");
        assert!(
            snapshot.messages.iter().any(|message| matches!(
                message,
                ConversationMessage::Assistant(message) if message.usage.is_some()
            )),
            "source history usage must remain unchanged"
        );

        let requests = model.take_requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.system,
            agent_model::SystemPromptSnapshot::new(vec![
                "normal agent instruction".to_owned(),
                "stable prefix".to_owned(),
            ])
        );
        let mut expected_conversation = snapshot.messages.clone();
        expected_conversation.push(compression_instruction_message());
        assert_eq!(request.conversation.messages, expected_conversation);
        assert!(request.tools.is_empty());
        assert_eq!(request.tool_choice, ToolChoice::None);
        assert_eq!(request.generation.max_output_tokens, Some(256));
        assert_eq!(request.generation.temperature, None);
        assert_eq!(request.generation.top_p, None);
        assert!(request.generation.stop.is_empty());
        assert_eq!(request.reasoning, None);
        assert!(request.provider_options.is_empty());
    }

    #[tokio::test]
    async fn rolling_summary_replaces_old_summary_and_preserves_complete_tool_tail() {
        let call_1 = ToolCallId::new("call_1").expect("valid call id");
        let call_2 = ToolCallId::new("call_2").expect("valid call id");
        let tool_name = ToolName::new("lookup").expect("valid tool name");
        let tool_turn = vec![
            user("user_2"),
            ConversationMessage::Assistant(AssistantMessage {
                id: id("assistant_tool_1"),
                model: model_identity(),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call_1.clone(),
                    name: tool_name.clone(),
                    arguments: serde_json::json!({"query": 1}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: Some(usage(50)),
            }),
            ConversationMessage::Tool(ToolMessage {
                id: id("tool_1"),
                result: ToolResult {
                    call_id: call_1,
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::text("one".to_owned()),
                    metadata: None,
                },
            }),
            ConversationMessage::Assistant(AssistantMessage {
                id: id("assistant_tool_2"),
                model: model_identity(),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call_2.clone(),
                    name: tool_name,
                    arguments: serde_json::json!({"query": 2}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: Some(usage(60)),
            }),
            ConversationMessage::Tool(ToolMessage {
                id: id("tool_2"),
                result: ToolResult {
                    call_id: call_2,
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::text("two".to_owned()),
                    metadata: None,
                },
            }),
            assistant("assistant_2", Some(usage(70))),
        ];
        let mut old_assistant = match assistant("assistant_1", Some(usage(40))) {
            ConversationMessage::Assistant(message) => message,
            _ => unreachable!("assistant helper always returns an assistant"),
        };
        old_assistant.parts.push(AssistantPart::ProviderState(
            OpaqueProviderState::new(
                ProviderId::new("deepseek").expect("provider"),
                ProtocolId::new("openai.responses").expect("protocol"),
                "responses.reasoning_item",
                "application/json",
                1,
                br#"{"type":"reasoning"}"#.to_vec(),
            )
            .expect("legacy provider state"),
        ));
        let mut messages = vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_old"),
                text: "old summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: None,
                programmatic_context: None,
            }),
            user("user_1"),
            ConversationMessage::Assistant(old_assistant),
        ];
        messages.extend(tool_turn.clone());
        let snapshot = ConversationSnapshot::new(messages);
        let summary = summary_message(
            "summary_new",
            vec![AssistantPart::Text(TextPart {
                id: part_id("summary_text"),
                text: "new rolling summary".to_owned(),
            })],
            FinishReason::Stop,
        );
        let model = Arc::new(ScriptedModel::new([Script::Events(message_events(
            &summary,
        ))]));
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(128, 1).expect("valid policy"));

        let outcome = strategy
            .compact(input(model.clone(), &snapshot), CancellationToken::new())
            .await
            .expect("rolling compaction succeeds");
        let StrategyOutcome::Candidate(candidate) = outcome else {
            panic!("expected candidate");
        };
        assert_eq!(candidate.report.compressed_blocks, 2);
        assert_eq!(candidate.report.retained_blocks, 1);
        assert!(matches!(
            candidate.replacement.messages.first(),
            Some(ConversationMessage::ContextSummary(summary))
                if summary.id == id("summary_new")
        ));
        assert_eq!(candidate.replacement.messages.len(), 1 + tool_turn.len());
        assert!(candidate.replacement.messages.iter().all(|message| {
            !matches!(message, ConversationMessage::Assistant(message) if message.parts.iter().any(|part| matches!(part, AssistantPart::ProviderState(_))))
        }));
        assert_eq!(
            candidate.replacement.messages[1], tool_turn[0],
            "tail user message stays at the same boundary"
        );
        assert_eq!(&candidate.replacement.messages[1..], tool_turn.as_slice());
        candidate
            .replacement
            .validate_tool_exchange_pairs()
            .expect("tool exchanges remain paired");
        let requests = model.take_requests();
        let mut expected_conversation = snapshot.messages.clone();
        expected_conversation.push(compression_instruction_message());
        assert_eq!(requests[0].conversation.messages, expected_conversation);
    }

    #[tokio::test]
    async fn no_compressible_head_returns_noop_without_model_call() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
        ]);
        let model = Arc::new(ScriptedModel::new([]));
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(64, 1).expect("valid policy"));

        let outcome = strategy
            .compact(input(model.clone(), &snapshot), CancellationToken::new())
            .await
            .expect("noop succeeds");
        assert_eq!(
            outcome,
            StrategyOutcome::NoOp {
                report: StrategyReport {
                    strategy: STRATEGY_NAME.to_owned(),
                    compressed_blocks: 0,
                    retained_blocks: 1,
                    model: None,
                    usage: None,
                }
            }
        );
        assert!(model.take_requests().is_empty());
    }

    #[tokio::test]
    async fn length_limited_summary_with_text_forms_a_candidate() {
        let mut old_assistant = assistant("assistant_1", Some(usage(20)));
        let ConversationMessage::Assistant(message) = &mut old_assistant else {
            unreachable!("assistant helper always returns an assistant");
        };
        let AssistantPart::Text(part) = &mut message.parts[0] else {
            unreachable!("assistant helper starts with text");
        };
        part.text = "old detail ".repeat(200);
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            old_assistant,
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let model = Arc::new(ScriptedModel::new([Script::Events(message_events(
            &summary_message(
                "summary_length",
                vec![AssistantPart::Text(TextPart {
                    id: part_id("text_length"),
                    text: "truncated but usable".to_owned(),
                })],
                FinishReason::Length,
            ),
        ))]));
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(64, 1).expect("valid policy"));

        assert!(matches!(
            strategy
                .compact(input(model, &snapshot), CancellationToken::new())
                .await,
            Ok(StrategyOutcome::Candidate(_))
        ));
    }

    #[tokio::test]
    async fn summary_that_does_not_reduce_snapshot_size_is_rejected() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let summary = summary_message(
            "summary_ineffective",
            vec![AssistantPart::Text(TextPart {
                id: part_id("summary_ineffective_text"),
                text: "summary expansion ".repeat(100),
            })],
            FinishReason::Stop,
        );
        let model = Arc::new(ScriptedModel::new([Script::Events(message_events(
            &summary,
        ))]));
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(256, 1).expect("valid policy"));

        let error = strategy
            .compact(input(model.clone(), &snapshot), CancellationToken::new())
            .await
            .expect_err("larger replacement must be rejected");
        assert!(matches!(
            error,
            CompactionError::Ineffective {
                source_bytes,
                replacement_bytes,
            } if replacement_bytes >= source_bytes
        ));
        assert_eq!(model.take_requests().len(), 1);
    }

    #[tokio::test]
    async fn compression_clones_the_normal_request_and_overrides_only_control_fields() {
        let mut old_assistant = assistant("assistant_1", Some(usage(20)));
        let ConversationMessage::Assistant(message) = &mut old_assistant else {
            unreachable!();
        };
        let AssistantPart::Text(part) = &mut message.parts[0] else {
            unreachable!();
        };
        part.text = "old detail ".repeat(200);
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            old_assistant,
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let summary = summary_message(
            "summary_options",
            vec![AssistantPart::Text(TextPart {
                id: part_id("summary_options_text"),
                text: "short summary".to_owned(),
            })],
            FinishReason::Stop,
        );
        let capabilities = ModelCapabilities {
            tool_calls: true,
            tool_choice: ToolChoiceCapabilities::all(),
            ..ModelCapabilities::default()
        };
        let model = Arc::new(
            ScriptedModel::new([Script::Events(message_events(&summary))])
                .with_capabilities(capabilities),
        );
        let mut compaction_input = input(model.clone(), &snapshot);
        compaction_input.normal_request.tools = vec![ToolDefinition {
            name: ToolName::new("lookup").expect("tool name"),
            description: "lookup".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        compaction_input.normal_request.tool_choice = ToolChoice::Auto;
        compaction_input.normal_request.generation.temperature = Some(0.3);
        compaction_input.normal_request.generation.top_p = Some(0.8);
        compaction_input.normal_request.generation.max_output_tokens = Some(99);
        compaction_input.normal_request.generation.stop = vec!["STOP".to_owned()];
        compaction_input.normal_request.reasoning = Some(ReasoningConfig {
            effort: Some(ReasoningEffort::High),
        });
        compaction_input
            .normal_request
            .provider_options
            .insert("test", serde_json::json!({"mode": "strict"}))
            .expect("provider options");
        let mut expected = compaction_input.normal_request.clone();
        expected
            .conversation
            .messages
            .push(compression_instruction_message());
        expected.tool_choice = ToolChoice::None;
        expected.generation.max_output_tokens = Some(512);

        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(512, 1).expect("valid policy"));
        assert!(matches!(
            strategy
                .compact(compaction_input, CancellationToken::new())
                .await,
            Ok(StrategyOutcome::Candidate(_))
        ));
        assert_eq!(model.take_requests(), vec![expected]);
    }

    #[tokio::test]
    async fn compression_fails_before_model_call_when_tools_cannot_be_suppressed() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let model = Arc::new(ScriptedModel::new([]));
        let mut compaction_input = input(model.clone(), &snapshot);
        compaction_input.normal_request.tools = vec![ToolDefinition {
            name: ToolName::new("lookup").expect("tool name"),
            description: "lookup".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(64, 1).expect("valid policy"));

        assert_eq!(
            strategy
                .compact(compaction_input, CancellationToken::new())
                .await,
            Err(CompactionError::UnsupportedToolSuppression)
        );
        assert!(model.take_requests().is_empty());
    }

    #[tokio::test]
    async fn structurally_invalid_summary_responses_never_form_candidates() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let invalid_messages = [
            summary_message(
                "summary_empty",
                vec![
                    AssistantPart::Reasoning(ReasoningPart {
                        id: part_id("reasoning_empty"),
                        text: "reasoning only".to_owned(),
                    }),
                    AssistantPart::Text(TextPart {
                        id: part_id("text_empty"),
                        text: "   ".to_owned(),
                    }),
                ],
                FinishReason::Stop,
            ),
            summary_message(
                "summary_tool",
                vec![
                    AssistantPart::Text(TextPart {
                        id: part_id("text_tool"),
                        text: "text".to_owned(),
                    }),
                    AssistantPart::ToolCall(ToolCall {
                        id: ToolCallId::new("call_summary").expect("valid call id"),
                        name: ToolName::new("lookup").expect("valid tool name"),
                        arguments: serde_json::json!({}),
                    }),
                ],
                FinishReason::Stop,
            ),
        ];

        for message in invalid_messages {
            let model = Arc::new(ScriptedModel::new([Script::Events(message_events(
                &message,
            ))]));
            let strategy = RollingSummarySameModel::new(
                RollingSummaryPolicy::new(64, 1).expect("valid policy"),
            );
            assert!(matches!(
                strategy
                    .compact(input(model, &snapshot), CancellationToken::new())
                    .await,
                Err(CompactionError::InvalidResponse { .. })
            ));
        }
    }

    #[tokio::test]
    async fn cancellation_and_model_errors_are_preserved_without_retry() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let strategy =
            RollingSummarySameModel::new(RollingSummaryPolicy::new(64, 1).expect("valid policy"));

        let pre_cancelled_model = Arc::new(ScriptedModel::new([]));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            strategy
                .compact(input(pre_cancelled_model.clone(), &snapshot), cancellation,)
                .await,
            Err(CompactionError::Cancelled)
        );
        assert!(pre_cancelled_model.take_requests().is_empty());

        let overflow = ModelError::ContextOverflow {
            message: "context limit".to_owned(),
        };
        let establishment_model = Arc::new(ScriptedModel::new([Script::EstablishmentError(
            overflow.clone(),
        )]));
        assert_eq!(
            strategy
                .compact(
                    input(establishment_model, &snapshot),
                    CancellationToken::new(),
                )
                .await,
            Err(CompactionError::Model(overflow.clone()))
        );

        let stream_model = Arc::new(ScriptedModel::new([Script::Events(vec![
            ModelEvent::TurnFailed {
                error: overflow.clone(),
            },
        ])]));
        assert_eq!(
            strategy
                .compact(input(stream_model, &snapshot), CancellationToken::new(),)
                .await,
            Err(CompactionError::Model(overflow))
        );

        let provider_error = ModelError::Provider {
            message: "rejected".to_owned(),
            status: Some(500),
        };
        let provider_model = Arc::new(ScriptedModel::new([Script::EstablishmentError(
            provider_error.clone(),
        )]));
        assert_eq!(
            strategy
                .compact(input(provider_model, &snapshot), CancellationToken::new(),)
                .await,
            Err(CompactionError::Model(provider_error))
        );

        let cancelled_stream_model = Arc::new(ScriptedModel::new([Script::Events(vec![
            ModelEvent::TurnFailed {
                error: ModelError::Cancelled,
            },
        ])]));
        assert_eq!(
            strategy
                .compact(
                    input(cancelled_stream_model, &snapshot),
                    CancellationToken::new(),
                )
                .await,
            Err(CompactionError::Cancelled)
        );
    }
}
