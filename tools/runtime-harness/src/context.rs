//! Temporary v0.3 context compaction orchestration for the Runtime Harness.
//!
//! The types and call shapes in this module are private verification scaffolding. They do not
//! define the future product `assistant-runtime` API.

use std::sync::Arc;

use agent_context::{
    CompactionError as StrategyCompactionError, CompactionInput, CompressionStrategy,
    ContextLayout, ContextLayoutError, ContextWindowDecision, ContextWindowError,
    ContextWindowEvaluation, ReplacementValidationError, StrategyOutcome, StrategyReport,
    validate_replacement,
};
use agent_core::{CompactionReason, ExecutionInput, ExecutionSpec};
#[cfg(test)]
use agent_types::ConversationMessage;
use agent_types::{ConversationSnapshot, UserMessage};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    journal::{HarnessContextCheckpoint, HarnessJournal, JournalError},
    runtime::HarnessRunId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HarnessCompactionCause {
    BeforeRunThreshold,
    InRunThreshold,
    ProviderOverflow,
    UserRequested,
}

impl HarnessCompactionCause {
    fn evaluates_trigger(self) -> bool {
        !matches!(self, Self::UserRequested)
    }
}

pub(crate) struct HarnessCompactionRequest {
    pub(crate) cause: HarnessCompactionCause,
    pub(crate) spec: Arc<ExecutionSpec>,
    pub(crate) cancellation: CancellationToken,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HarnessCompactionReport {
    pub(crate) cause: HarnessCompactionCause,
    pub(crate) strategy: StrategyReport,
    pub(crate) trigger: Option<ContextWindowEvaluation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum HarnessCompactionOutcome {
    Compacted {
        checkpoint: HarnessContextCheckpoint,
        report: HarnessCompactionReport,
    },
    NoOp {
        report: HarnessCompactionReport,
    },
}

#[derive(Clone, Debug, Error, PartialEq)]
pub(crate) enum HarnessCompactionError {
    #[error("harness context compaction was cancelled")]
    Cancelled,
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Layout(#[from] ContextLayoutError),
    #[error(transparent)]
    Window(#[from] ContextWindowError),
    #[error(transparent)]
    Strategy(StrategyCompactionError),
    #[error(transparent)]
    InvalidReplacement(#[from] ReplacementValidationError),
}

pub(crate) struct HarnessContextCoordinator {
    journal: Arc<HarnessJournal>,
    strategy: Arc<dyn CompressionStrategy>,
}

impl HarnessContextCoordinator {
    pub(crate) fn new(
        journal: Arc<HarnessJournal>,
        strategy: Arc<dyn CompressionStrategy>,
    ) -> Self {
        Self { journal, strategy }
    }

    #[cfg(test)]
    pub(crate) fn journal(&self) -> &Arc<HarnessJournal> {
        &self.journal
    }

    pub(crate) async fn compact_context(
        &self,
        request: HarnessCompactionRequest,
    ) -> Result<HarnessCompactionOutcome, HarnessCompactionError> {
        ensure_not_cancelled(&request.cancellation)?;
        let snapshot = self.journal.effective_snapshot()?;
        ensure_not_cancelled(&request.cancellation)?;

        let trigger = if request.cause.evaluates_trigger() {
            Some(
                request
                    .spec
                    .context_window
                    .evaluate(&snapshot, request.spec.model.as_ref())?,
            )
        } else {
            None
        };
        let layout = ContextLayout::build(&snapshot)?;
        let strategy_outcome = self
            .strategy
            .compact(
                CompactionInput {
                    model: Arc::clone(&request.spec.model),
                    system_prompt: request.spec.system_prompt.clone(),
                    layout,
                },
                request.cancellation.clone(),
            )
            .await
            .map_err(map_strategy_error)?;

        match strategy_outcome {
            StrategyOutcome::Candidate(candidate) => {
                ensure_not_cancelled(&request.cancellation)?;
                validate_replacement(&candidate.replacement)?;
                let checkpoint = HarnessContextCheckpoint {
                    replacement: candidate.replacement,
                };
                self.journal.commit_checkpoint(checkpoint.clone())?;
                Ok(HarnessCompactionOutcome::Compacted {
                    checkpoint,
                    report: HarnessCompactionReport {
                        cause: request.cause,
                        strategy: candidate.report,
                        trigger,
                    },
                })
            }
            StrategyOutcome::NoOp { report } => Ok(HarnessCompactionOutcome::NoOp {
                report: HarnessCompactionReport {
                    cause: request.cause,
                    strategy: report,
                    trigger,
                },
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HarnessRunContextKind {
    Initial,
    Continuation { previous_run_id: HarnessRunId },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HarnessPreparedContext {
    pub(crate) kind: HarnessRunContextKind,
    pub(crate) input: ExecutionInput,
    pub(crate) preflight: Option<ContextWindowEvaluation>,
    pub(crate) compaction: Option<HarnessCompactionReport>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HarnessCompactionRequiredRun {
    pub(crate) run_id: HarnessRunId,
    pub(crate) reason: CompactionReason,
    pub(crate) step: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HarnessTaskChain {
    automatic_compactions: u32,
    max_automatic_compactions: u32,
}

impl HarnessTaskChain {
    pub(crate) fn new(max_automatic_compactions: u32) -> Self {
        Self {
            automatic_compactions: 0,
            max_automatic_compactions,
        }
    }

    pub(crate) fn automatic_compactions(&self) -> u32 {
        self.automatic_compactions
    }

    pub(crate) fn max_automatic_compactions(&self) -> u32 {
        self.max_automatic_compactions
    }

    pub(crate) fn reset(&mut self) {
        self.automatic_compactions = 0;
    }

    fn ensure_automatic_compaction_available(&self) -> Result<(), HarnessRunPreparationError> {
        if self.automatic_compactions >= self.max_automatic_compactions {
            Err(
                HarnessRunPreparationError::AutomaticCompactionLimitReached {
                    limit: self.max_automatic_compactions,
                },
            )
        } else {
            Ok(())
        }
    }

    fn record_automatic_compaction(&mut self) {
        self.automatic_compactions += 1;
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub(crate) enum HarnessRunPreparationError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Window(#[from] ContextWindowError),
    #[error(transparent)]
    Compaction(#[from] HarnessCompactionError),
    #[error("automatic harness context compaction produced no replacement")]
    CompactionNoOp {
        report: Box<HarnessCompactionReport>,
    },
    #[error("automatic harness context compaction limit reached: {limit}")]
    AutomaticCompactionLimitReached { limit: u32 },
}

pub(crate) async fn prepare_user_context(
    coordinator: &HarnessContextCoordinator,
    task: &mut HarnessTaskChain,
    user: UserMessage,
    spec: Arc<ExecutionSpec>,
    cancellation: CancellationToken,
) -> Result<HarnessPreparedContext, HarnessRunPreparationError> {
    coordinator.journal.append_user(user)?;
    let snapshot = coordinator.journal.effective_snapshot()?;
    let evaluation = spec
        .context_window
        .evaluate(&snapshot, spec.model.as_ref())?;

    match evaluation.decision {
        ContextWindowDecision::Ready | ContextWindowDecision::UsageUnavailable => {
            Ok(HarnessPreparedContext {
                kind: HarnessRunContextKind::Initial,
                input: ExecutionInput {
                    conversation: snapshot,
                },
                preflight: Some(evaluation),
                compaction: None,
            })
        }
        ContextWindowDecision::CompactionRequired => {
            let (replacement, report) = compact_automatically(
                coordinator,
                task,
                HarnessCompactionCause::BeforeRunThreshold,
                spec,
                cancellation,
            )
            .await?;
            Ok(HarnessPreparedContext {
                kind: HarnessRunContextKind::Initial,
                input: ExecutionInput {
                    conversation: replacement,
                },
                preflight: Some(evaluation),
                compaction: Some(report),
            })
        }
    }
}

pub(crate) async fn handle_compaction_required(
    coordinator: &HarnessContextCoordinator,
    task: &mut HarnessTaskChain,
    completed_run: &HarnessCompactionRequiredRun,
    spec: Arc<ExecutionSpec>,
    cancellation: CancellationToken,
) -> Result<HarnessPreparedContext, HarnessRunPreparationError> {
    let cause = match completed_run.reason {
        CompactionReason::ThresholdReached => HarnessCompactionCause::InRunThreshold,
        CompactionReason::ProviderOverflow => HarnessCompactionCause::ProviderOverflow,
    };
    let (replacement, report) =
        compact_automatically(coordinator, task, cause, spec, cancellation).await?;
    Ok(HarnessPreparedContext {
        kind: HarnessRunContextKind::Continuation {
            previous_run_id: completed_run.run_id.clone(),
        },
        input: ExecutionInput {
            conversation: replacement,
        },
        preflight: None,
        compaction: Some(report),
    })
}

pub(crate) async fn handle_user_compaction(
    coordinator: &HarnessContextCoordinator,
    spec: Arc<ExecutionSpec>,
    cancellation: CancellationToken,
) -> Result<HarnessCompactionOutcome, HarnessCompactionError> {
    coordinator
        .compact_context(HarnessCompactionRequest {
            cause: HarnessCompactionCause::UserRequested,
            spec,
            cancellation,
        })
        .await
}

async fn compact_automatically(
    coordinator: &HarnessContextCoordinator,
    task: &mut HarnessTaskChain,
    cause: HarnessCompactionCause,
    spec: Arc<ExecutionSpec>,
    cancellation: CancellationToken,
) -> Result<(ConversationSnapshot, HarnessCompactionReport), HarnessRunPreparationError> {
    task.ensure_automatic_compaction_available()?;
    match coordinator
        .compact_context(HarnessCompactionRequest {
            cause,
            spec,
            cancellation,
        })
        .await?
    {
        HarnessCompactionOutcome::Compacted { checkpoint, report } => {
            task.record_automatic_compaction();
            Ok((checkpoint.replacement, report))
        }
        HarnessCompactionOutcome::NoOp { report } => {
            Err(HarnessRunPreparationError::CompactionNoOp {
                report: Box::new(report),
            })
        }
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), HarnessCompactionError> {
    if cancellation.is_cancelled() {
        Err(HarnessCompactionError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_strategy_error(error: StrategyCompactionError) -> HarnessCompactionError {
    match error {
        StrategyCompactionError::Cancelled => HarnessCompactionError::Cancelled,
        other => HarnessCompactionError::Strategy(other),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use agent_context::{
        CompactionCandidate, CompactionError as StrategyCompactionError, CompactionFuture,
        CompactionInput, CompressionStrategy, ContextWindowEvaluator, StrategyOutcome,
        StrategyReport,
    };
    use agent_core::{CompactionReason, ExecutionBudget, ExecutionSpec};
    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelService,
        ModelStreamFuture,
    };
    use agent_types::{
        AssistantMessage, ContextSummaryMessage, FinishReason, MessageId, ModelIdentity, PartId,
        ProviderId, TextPart, TokenUsage, UserPart,
    };

    use super::*;
    use crate::journal::{
        ConversationRecord, effective_snapshot_from_records, original_messages_from_records,
    };

    struct WindowModel {
        capabilities: ModelCapabilities,
        context_window_tokens: u64,
    }

    impl ModelService for WindowModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            self.context_window_tokens
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(std::future::ready(Err(ModelError::Config(
                "harness context test model does not stream".to_owned(),
            ))))
        }
    }

    fn spec(threshold: f64) -> Arc<ExecutionSpec> {
        Arc::new(ExecutionSpec {
            system_prompt: agent_model::SystemPromptSnapshot::new(vec![
                "normal instruction".to_owned(),
            ]),
            model: Arc::new(WindowModel {
                capabilities: ModelCapabilities::default(),
                context_window_tokens: 100,
            }),
            context_window: Arc::new(
                ContextWindowEvaluator::new(threshold).expect("valid threshold"),
            ),
            tools: Default::default(),
            budget: ExecutionBudget::default(),
            guardrails: None,
        })
    }

    struct ScriptedStrategy {
        outcomes: Mutex<VecDeque<Result<StrategyOutcome, StrategyCompactionError>>>,
        calls: AtomicUsize,
    }

    impl ScriptedStrategy {
        fn new(
            outcomes: impl IntoIterator<Item = Result<StrategyOutcome, StrategyCompactionError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl CompressionStrategy for ScriptedStrategy {
        fn compact<'a>(
            &'a self,
            _input: CompactionInput,
            cancellation: CancellationToken,
        ) -> CompactionFuture<'a> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(StrategyCompactionError::Cancelled);
                }
                self.outcomes
                    .lock()
                    .expect("strategy lock")
                    .pop_front()
                    .expect("scripted strategy outcome")
            })
        }
    }

    fn id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message id")
    }

    fn user(value: &str) -> UserMessage {
        UserMessage {
            id: id(value),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("{value}_text")).expect("valid part id"),
                text: value.to_owned(),
            })],
        }
    }

    fn assistant(value: &str, total_tokens: u64) -> AssistantMessage {
        AssistantMessage {
            id: id(value),
            model: ModelIdentity::new(
                ProviderId::new("test").expect("valid provider id"),
                "test-model",
            ),
            parts: vec![],
            finish_reason: FinishReason::Stop,
            usage: Some(TokenUsage {
                input_tokens: total_tokens,
                output_tokens: 0,
                total_tokens,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
        }
    }

    fn history(total_tokens: u64) -> Vec<ConversationMessage> {
        vec![
            ConversationMessage::User(user("user_1")),
            ConversationMessage::Assistant(assistant("assistant_1", total_tokens)),
        ]
    }

    fn journal(total_tokens: u64) -> Arc<HarnessJournal> {
        let journal = HarnessJournal::new();
        journal.append_user(user("user_1")).expect("append user");
        journal
            .append_assistant(assistant("assistant_1", total_tokens))
            .expect("append assistant");
        journal
    }

    fn replacement(latest_user: &str) -> ConversationSnapshot {
        ConversationSnapshot::new(vec![
            ConversationMessage::ContextSummary(ContextSummaryMessage {
                id: id("summary_1"),
                text: "earlier conversation summary".to_owned(),
            }),
            ConversationMessage::User(user(latest_user)),
        ])
    }

    fn report() -> StrategyReport {
        StrategyReport {
            strategy: "scripted".to_owned(),
            compressed_blocks: 1,
            retained_blocks: 1,
            model: None,
            usage: None,
        }
    }

    fn candidate(latest_user: &str) -> StrategyOutcome {
        StrategyOutcome::Candidate(CompactionCandidate {
            replacement: replacement(latest_user),
            report: report(),
        })
    }

    fn noop() -> StrategyOutcome {
        StrategyOutcome::NoOp { report: report() }
    }

    fn coordinator(
        journal: Arc<HarnessJournal>,
        strategy: Arc<ScriptedStrategy>,
    ) -> HarnessContextCoordinator {
        HarnessContextCoordinator::new(journal, strategy)
    }

    #[test]
    fn latest_checkpoint_drives_effective_projection_without_hiding_original_messages() {
        let first_checkpoint = HarnessContextCheckpoint {
            replacement: ConversationSnapshot::new(vec![ConversationMessage::ContextSummary(
                ContextSummaryMessage {
                    id: id("summary_old"),
                    text: "old summary".to_owned(),
                },
            )]),
        };
        let latest_checkpoint = HarnessContextCheckpoint {
            replacement: ConversationSnapshot::new(vec![ConversationMessage::ContextSummary(
                ContextSummaryMessage {
                    id: id("summary_new"),
                    text: "new summary".to_owned(),
                },
            )]),
        };
        let records = vec![
            ConversationRecord::Message(ConversationMessage::User(user("user_1"))),
            ConversationRecord::Checkpoint(first_checkpoint),
            ConversationRecord::Message(ConversationMessage::User(user("user_2"))),
            ConversationRecord::Checkpoint(latest_checkpoint.clone()),
            ConversationRecord::Message(ConversationMessage::User(user("user_3"))),
        ];

        assert_eq!(
            effective_snapshot_from_records(&records),
            ConversationSnapshot::new(vec![
                latest_checkpoint.replacement.messages[0].clone(),
                ConversationMessage::User(user("user_3")),
            ])
        );
        assert_eq!(
            original_messages_from_records(&records),
            ConversationSnapshot::new(vec![
                ConversationMessage::User(user("user_1")),
                ConversationMessage::User(user("user_2")),
                ConversationMessage::User(user("user_3")),
            ])
        );
    }

    #[tokio::test]
    async fn four_causes_share_one_harness_compaction_core() {
        let journal = journal(90);
        let strategy = ScriptedStrategy::new([
            Ok(candidate("tail_1")),
            Ok(candidate("tail_2")),
            Ok(candidate("tail_3")),
            Ok(candidate("tail_4")),
        ]);
        let coordinator = coordinator(Arc::clone(&journal), Arc::clone(&strategy));

        for cause in [
            HarnessCompactionCause::BeforeRunThreshold,
            HarnessCompactionCause::InRunThreshold,
            HarnessCompactionCause::ProviderOverflow,
            HarnessCompactionCause::UserRequested,
        ] {
            let outcome = coordinator
                .compact_context(HarnessCompactionRequest {
                    cause,
                    spec: spec(0.8),
                    cancellation: CancellationToken::new(),
                })
                .await
                .expect("compaction");
            let HarnessCompactionOutcome::Compacted { report, .. } = outcome else {
                panic!("candidate must commit");
            };
            assert_eq!(report.cause, cause);
            assert_eq!(
                report.trigger.is_some(),
                cause != HarnessCompactionCause::UserRequested
            );
        }

        assert_eq!(strategy.calls(), 4);
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 4);
    }

    #[tokio::test]
    async fn invalid_candidate_and_commit_failure_leave_no_checkpoint() {
        let journal = journal(90);
        let invalid = StrategyOutcome::Candidate(CompactionCandidate {
            replacement: ConversationSnapshot::default(),
            report: report(),
        });
        let strategy = ScriptedStrategy::new([Ok(invalid), Ok(candidate("user_after_summary"))]);
        let coordinator = coordinator(Arc::clone(&journal), strategy);

        assert!(matches!(
            coordinator
                .compact_context(HarnessCompactionRequest {
                    cause: HarnessCompactionCause::UserRequested,
                    spec: spec(0.8),
                    cancellation: CancellationToken::new(),
                })
                .await,
            Err(HarnessCompactionError::InvalidReplacement(_))
        ));
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 0);

        journal.set_fail_checkpoint_commit(true);
        assert_eq!(
            coordinator
                .compact_context(HarnessCompactionRequest {
                    cause: HarnessCompactionCause::UserRequested,
                    spec: spec(0.8),
                    cancellation: CancellationToken::new(),
                })
                .await,
            Err(HarnessCompactionError::Journal(JournalError::Injected(
                "checkpoint commit"
            )))
        );
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 0);
    }

    #[tokio::test]
    async fn journal_read_failure_and_pre_cancel_skip_strategy() {
        let journal = journal(90);
        let strategy = ScriptedStrategy::new([Ok(candidate("tail"))]);
        let coordinator = coordinator(Arc::clone(&journal), Arc::clone(&strategy));
        journal.set_fail_effective_snapshot(true);
        assert_eq!(
            handle_user_compaction(&coordinator, spec(0.8), CancellationToken::new()).await,
            Err(HarnessCompactionError::Journal(JournalError::Injected(
                "effective snapshot"
            )))
        );
        assert_eq!(strategy.calls(), 0);

        journal.set_fail_effective_snapshot(false);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            handle_user_compaction(&coordinator, spec(0.8), cancellation).await,
            Err(HarnessCompactionError::Cancelled)
        );
        assert_eq!(strategy.calls(), 0);
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 0);
    }

    #[tokio::test]
    async fn ready_user_input_is_appended_before_initial_context_is_prepared() {
        let journal = journal(20);
        let strategy = ScriptedStrategy::new([]);
        let coordinator = coordinator(Arc::clone(&journal), strategy);
        let mut task = HarnessTaskChain::new(2);

        let prepared = prepare_user_context(
            &coordinator,
            &mut task,
            user("user_2"),
            spec(0.8),
            CancellationToken::new(),
        )
        .await
        .expect("ready context");

        assert_eq!(prepared.kind, HarnessRunContextKind::Initial);
        assert_eq!(prepared.compaction, None);
        assert_eq!(
            prepared.input.conversation.messages.last(),
            Some(&ConversationMessage::User(user("user_2")))
        );
        assert_eq!(task.automatic_compactions(), 0);
    }

    #[tokio::test]
    async fn before_run_threshold_commits_before_initial_context_is_prepared() {
        let journal = journal(90);
        let strategy = ScriptedStrategy::new([Ok(candidate("user_2"))]);
        let coordinator = coordinator(Arc::clone(&journal), Arc::clone(&strategy));
        let mut task = HarnessTaskChain::new(2);

        let prepared = prepare_user_context(
            &coordinator,
            &mut task,
            user("user_2"),
            spec(0.8),
            CancellationToken::new(),
        )
        .await
        .expect("compacted context");

        assert_eq!(prepared.kind, HarnessRunContextKind::Initial);
        assert_eq!(prepared.input.conversation, replacement("user_2"));
        assert_eq!(
            prepared
                .compaction
                .as_ref()
                .expect("compaction report")
                .cause,
            HarnessCompactionCause::BeforeRunThreshold
        );
        assert_eq!(task.automatic_compactions(), 1);
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 1);
        assert_eq!(
            journal
                .snapshot()
                .expect("original history")
                .conversation
                .messages,
            [history(90), vec![ConversationMessage::User(user("user_2"))]].concat()
        );
    }

    #[tokio::test]
    async fn automatic_noop_failure_and_cancel_prepare_no_context() {
        let noop_journal = journal(90);
        let noop_coordinator = coordinator(
            Arc::clone(&noop_journal),
            ScriptedStrategy::new([Ok(noop())]),
        );
        let mut noop_task = HarnessTaskChain::new(2);
        assert!(matches!(
            prepare_user_context(
                &noop_coordinator,
                &mut noop_task,
                user("user_2"),
                spec(0.8),
                CancellationToken::new(),
            )
            .await,
            Err(HarnessRunPreparationError::CompactionNoOp { .. })
        ));
        assert_eq!(noop_task.automatic_compactions(), 0);

        let failure_journal = journal(90);
        let failure_coordinator = coordinator(
            Arc::clone(&failure_journal),
            ScriptedStrategy::new([Err(StrategyCompactionError::Model(ModelError::Config(
                "injected strategy failure".to_owned(),
            )))]),
        );
        let mut failure_task = HarnessTaskChain::new(2);
        assert!(matches!(
            prepare_user_context(
                &failure_coordinator,
                &mut failure_task,
                user("user_2"),
                spec(0.8),
                CancellationToken::new(),
            )
            .await,
            Err(HarnessRunPreparationError::Compaction(
                HarnessCompactionError::Strategy(_)
            ))
        ));
        assert_eq!(failure_task.automatic_compactions(), 0);
        assert_eq!(
            failure_journal
                .checkpoint_count()
                .expect("checkpoint count"),
            0
        );

        let cancel_journal = journal(90);
        let cancel_strategy = ScriptedStrategy::new([Ok(candidate("user_2"))]);
        let cancel_coordinator =
            coordinator(Arc::clone(&cancel_journal), Arc::clone(&cancel_strategy));
        let mut cancel_task = HarnessTaskChain::new(2);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            prepare_user_context(
                &cancel_coordinator,
                &mut cancel_task,
                user("user_2"),
                spec(0.8),
                cancellation,
            )
            .await,
            Err(HarnessRunPreparationError::Compaction(
                HarnessCompactionError::Cancelled
            ))
        );
        assert_eq!(cancel_strategy.calls(), 0);
        assert_eq!(cancel_task.automatic_compactions(), 0);
        assert_eq!(
            cancel_journal.checkpoint_count().expect("checkpoint count"),
            0
        );
    }

    #[tokio::test]
    async fn compaction_required_maps_reason_without_appending_user_message() {
        for (reason, expected_cause) in [
            (
                CompactionReason::ThresholdReached,
                HarnessCompactionCause::InRunThreshold,
            ),
            (
                CompactionReason::ProviderOverflow,
                HarnessCompactionCause::ProviderOverflow,
            ),
        ] {
            let journal = journal(90);
            let coordinator = coordinator(
                Arc::clone(&journal),
                ScriptedStrategy::new([Ok(candidate("user_2"))]),
            );
            let mut task = HarnessTaskChain::new(2);
            let completed = HarnessCompactionRequiredRun {
                run_id: HarnessRunId::from_sequence(7),
                reason,
                step: 3,
            };

            let continuation = handle_compaction_required(
                &coordinator,
                &mut task,
                &completed,
                spec(0.8),
                CancellationToken::new(),
            )
            .await
            .expect("continuation context");

            assert_eq!(completed.reason, reason);
            assert_eq!(completed.step, 3);
            assert_eq!(
                continuation.kind,
                HarnessRunContextKind::Continuation {
                    previous_run_id: HarnessRunId::from_sequence(7),
                }
            );
            assert_eq!(
                continuation
                    .compaction
                    .as_ref()
                    .expect("continuation report")
                    .cause,
                expected_cause
            );
            assert_eq!(
                journal
                    .snapshot()
                    .expect("original history")
                    .conversation
                    .messages,
                history(90)
            );
        }
    }

    #[tokio::test]
    async fn automatic_limit_stops_before_strategy() {
        let journal = journal(90);
        let strategy = ScriptedStrategy::new([Ok(candidate("user_2"))]);
        let coordinator = coordinator(Arc::clone(&journal), Arc::clone(&strategy));
        let mut task = HarnessTaskChain::new(0);
        let completed = HarnessCompactionRequiredRun {
            run_id: HarnessRunId::from_sequence(1),
            reason: CompactionReason::ProviderOverflow,
            step: 1,
        };

        assert_eq!(
            handle_compaction_required(
                &coordinator,
                &mut task,
                &completed,
                spec(0.8),
                CancellationToken::new(),
            )
            .await,
            Err(HarnessRunPreparationError::AutomaticCompactionLimitReached { limit: 0 })
        );
        assert_eq!(strategy.calls(), 0);
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 0);
    }

    #[tokio::test]
    async fn user_compaction_has_no_task_or_run_plan_inputs() {
        let journal = journal(20);
        let coordinator = coordinator(Arc::clone(&journal), ScriptedStrategy::new([Ok(noop())]));

        let outcome = handle_user_compaction(&coordinator, spec(0.8), CancellationToken::new())
            .await
            .expect("user noop");
        let HarnessCompactionOutcome::NoOp { report } = outcome else {
            panic!("script returns noop");
        };
        assert_eq!(report.cause, HarnessCompactionCause::UserRequested);
        assert_eq!(report.trigger, None);
        assert_eq!(journal.checkpoint_count().expect("checkpoint count"), 0);
    }

    #[test]
    fn checkpoint_payload_contains_only_replacement() {
        let checkpoint = HarnessContextCheckpoint {
            replacement: replacement("user_2"),
        };
        let json = serde_json::to_value(&checkpoint).expect("serialize checkpoint");
        let keys = json
            .as_object()
            .expect("checkpoint object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["replacement"]);
    }

    #[test]
    fn coordinator_exposes_the_same_temporary_journal() {
        let journal = journal(20);
        let coordinator = coordinator(
            Arc::clone(&journal),
            ScriptedStrategy::new(Vec::<Result<StrategyOutcome, StrategyCompactionError>>::new()),
        );
        assert!(Arc::ptr_eq(coordinator.journal(), &journal));
        assert_eq!(journal.records().len(), 2);
    }
}
