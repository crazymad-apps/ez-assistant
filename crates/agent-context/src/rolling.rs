//! 同模型 Rolling Summary 的配置与策略实现。

use agent_model::{
    GenerationConfig, LifecycleValidator, ModelCallContext, ModelError, ModelEvent, ModelRequest,
    ProviderOptions,
};
use agent_types::{
    AssistantMessage, AssistantPart, ContextSummaryMessage, ConversationMessage,
    ConversationSnapshot, FinishReason, MessageId, PartId, TextPart, ToolChoice, UserMessage,
    UserPart,
};
use futures_util::StreamExt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    CompactionCandidate, CompactionError, CompactionFuture, CompactionInput, CompressionStrategy,
    StrategyOutcome, StrategyReport, validate_replacement,
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
                system_prompt,
                layout,
            } = input;
            let partition = layout.partition(policy.minimum_recent_user_turns());
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

            let request = ModelRequest {
                // 保持正常请求的 system prompt 不变，使 Provider 可以复用前缀缓存。
                system: system_prompt,
                conversation: build_compression_conversation(
                    partition.protected_prefix(),
                    partition.compressible_head(),
                ),
                tools: vec![],
                tool_choice: ToolChoice::None,
                generation: GenerationConfig {
                    max_output_tokens: Some(policy.summary_output_tokens()),
                    ..GenerationConfig::default()
                },
                reasoning: None,
                provider_options: ProviderOptions::new(),
            };
            let stream = model
                .stream(request, ModelCallContext::new(cancellation.clone()))
                .await
                .map_err(map_model_error)?;
            let mut stream = LifecycleValidator::new(stream);
            let message = loop {
                let Some(event) = stream.next().await else {
                    return Err(CompactionError::InvalidResponse {
                        message: "compression stream ended without a terminal event".to_owned(),
                    });
                };
                match event {
                    ModelEvent::TurnFinished { message } => break message,
                    ModelEvent::TurnFailed { error } => return Err(map_model_error(error)),
                    _ => {}
                }
            };
            if cancellation.is_cancelled() {
                return Err(CompactionError::Cancelled);
            }

            let summary_text = summary_text(&message)?;
            let replacement = build_replacement(
                partition.protected_prefix(),
                partition.protected_tail(),
                &message,
                summary_text,
            );
            validate_replacement(&replacement)?;

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

fn build_compression_conversation(
    protected_prefix: &[ConversationMessage],
    compressible_head: &[crate::ContextBlock],
) -> ConversationSnapshot {
    let mut messages = protected_prefix.to_vec();
    messages.extend(
        compressible_head
            .iter()
            .flat_map(|block| block.messages().iter().cloned()),
    );
    messages.push(compression_instruction_message());
    ConversationSnapshot::new(messages)
}

fn compression_instruction_message() -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: MessageId::new(COMPRESSION_MESSAGE_ID)
            .expect("static compression message id must be valid"),
        parts: vec![UserPart::Injected(TextPart {
            id: PartId::new(COMPRESSION_PART_ID).expect("static compression part id must be valid"),
            text: COMPRESSION_INSTRUCTIONS.to_owned(),
        })],
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
    if message.finish_reason != FinishReason::Stop {
        return Err(CompactionError::InvalidResponse {
            message: "compression response did not finish with stop".to_owned(),
        });
    }
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
    protected_tail: &[crate::ContextBlock],
    message: &AssistantMessage,
    summary_text: String,
) -> ConversationSnapshot {
    let mut messages = protected_prefix.to_vec();
    messages.push(ConversationMessage::ContextSummary(ContextSummaryMessage {
        id: message.id.clone(),
        text: summary_text,
    }));
    messages.extend(
        protected_tail
            .iter()
            .flat_map(|block| block.messages().iter().cloned())
            .map(clear_assistant_usage),
    );
    ConversationSnapshot::new(messages)
}

fn clear_assistant_usage(mut message: ConversationMessage) -> ConversationMessage {
    if let ConversationMessage::Assistant(assistant) = &mut message {
        assistant.usage = None;
    }
    message
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

    use agent_model::{ModelCapabilities, ModelEventStream, ModelService, ModelStreamFuture};
    use agent_types::{
        AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId, ReasoningPart,
        SystemMessage, TextPart, TokenUsage, ToolCall, ToolCallId, ToolMessage, ToolName,
        ToolResult, ToolResultContent, ToolResultStatus, UserMessage,
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
            system_prompt: agent_model::SystemPromptSnapshot::new(vec![
                "normal agent instruction".to_owned(),
                "stable prefix".to_owned(),
            ]),
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
                }),
                snapshot.messages[5].clone(),
                assistant("assistant_3", None),
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
        let mut expected_conversation = snapshot.messages[..5].to_vec();
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
                    content: ToolResultContent::Text("one".to_owned()),
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
                    content: ToolResultContent::Text("two".to_owned()),
                },
            }),
            assistant("assistant_2", Some(usage(70))),
        ];
        let mut messages = vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_old"),
                text: "old summary".to_owned(),
            }),
            user("user_1"),
            assistant("assistant_1", Some(usage(40))),
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
        assert_eq!(
            candidate.replacement.messages[1], tool_turn[0],
            "tail user message stays at the same boundary"
        );
        for message in &candidate.replacement.messages[1..] {
            if let ConversationMessage::Assistant(message) = message {
                assert_eq!(message.usage, None);
            }
        }
        candidate
            .replacement
            .validate_tool_exchange_pairs()
            .expect("tool exchanges remain paired");
        let requests = model.take_requests();
        let mut expected_conversation = snapshot.messages[..3].to_vec();
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
    async fn invalid_summary_responses_never_form_candidates() {
        let snapshot = ConversationSnapshot::new(vec![
            user("user_1"),
            assistant("assistant_1", Some(usage(20))),
            user("user_2"),
            assistant("assistant_2", Some(usage(30))),
        ]);
        let invalid_messages = [
            summary_message(
                "summary_length",
                vec![AssistantPart::Text(TextPart {
                    id: part_id("text_length"),
                    text: "truncated".to_owned(),
                })],
                FinishReason::Length,
            ),
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
