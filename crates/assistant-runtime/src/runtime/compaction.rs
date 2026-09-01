//! 独立手动 compact 命令与自动/手动共用的 Session 易失压缩状态。

use std::sync::Arc;

use agent_types::ConversationSnapshot;
use assistant_protocol::{
    CancelSessionCompactionRequest, CancelSessionCompactionResult, CompactSessionOutcome,
    CompactSessionRequest, CompactSessionResult, RuntimeEvent, SessionCompactionFinishedOutcome,
    SessionCompactionSnapshot, SessionCompactionTriggerSnapshot,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::AssistantRuntime;
use crate::{
    ContextReplacement, ContextReplacementTarget, RuntimeError, RuntimeResult, RuntimeStore,
    SessionHistoryCompactionFinish, SessionHistoryCompactionFinishKind,
    SessionHistoryCompactionPreparation, SessionHistoryCompactionPreparationResult, StoreErrorKind,
    context_compaction::{
        ManualCompactionCandidate, RuntimeContextCompactor, SessionCompactionGuard,
    },
    observation::ObservationCoordinator,
    session::SessionController,
};

struct ManualCompactionContext {
    session: Arc<SessionController>,
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
    compactor: RuntimeContextCompactor,
    operation_id: assistant_protocol::IdempotencyKey,
    source_generation: u64,
    source: ConversationSnapshot,
    cancellation: CancellationToken,
    guard: SessionCompactionGuard,
}

impl AssistantRuntime {
    /// 手动 compact 是 Runtime 自持有任务；HTTP future 丢失不会中止已登记的操作。
    pub async fn compact_session(
        &self,
        request: CompactSessionRequest,
    ) -> RuntimeResult<CompactSessionResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let mutation = session.mutation().await;
        session.ensure_healthy()?;
        session.ensure_active()?;
        session.ensure_idle()?;
        if self
            .child_tasks
            .active_count_for_session(&request.session_id)?
            != 0
            || !self.approval_registry.list(&request.session_id)?.is_empty()
        {
            return Err(RuntimeError::SessionNotIdle {
                session_id: request.session_id,
            });
        }
        let compactor = self.compile_session_compactor(&session)?;
        let prepared_at_ms = super::now_ms()?;
        match self
            .store
            .prepare_session_compaction(SessionHistoryCompactionPreparation {
                operation_id: request.operation_id.clone(),
                session_id: request.session_id.clone(),
                expected_generation: request.expected_generation,
                created_at_ms: prepared_at_ms,
            })
            .await
            .map_err(|source| {
                if source.kind() == StoreErrorKind::Conflict {
                    RuntimeError::SnapshotStale
                } else {
                    RuntimeError::from_store("prepare session compaction", source)
                }
            })? {
            SessionHistoryCompactionPreparationResult::Completed(outcome) => {
                return Ok(CompactSessionResult {
                    session: session.summary()?,
                    outcome,
                });
            }
            SessionHistoryCompactionPreparationResult::Prepared => {}
        }

        let source_state = (|| -> RuntimeResult<(ConversationSnapshot, u64)> {
            let state = session.lock_state()?;
            let journal = state
                .journal
                .as_ref()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "manual compaction conversation",
                })?;
            if journal.has_pending() || state.body_generation != request.expected_generation {
                return Err(RuntimeError::SnapshotStale);
            }
            Ok((journal.snapshot(), state.body_generation))
        })();
        let (source, source_generation) = match source_state {
            Ok(source) => source,
            Err(error) => {
                let _ = self
                    .store
                    .finish_session_compaction(SessionHistoryCompactionFinish {
                        operation_id: request.operation_id,
                        session_id: request.session_id,
                        expected_generation: request.expected_generation,
                        kind: SessionHistoryCompactionFinishKind::Interrupted,
                        finished_at_ms: prepared_at_ms,
                    })
                    .await;
                return Err(error);
            }
        };
        let cancellation = self.root_cancellation.child_token();
        let guard = match SessionCompactionGuard::begin(
            session.clone(),
            self.event_sender.clone(),
            SessionCompactionSnapshot {
                compaction_id: request.operation_id.as_str().to_owned(),
                trigger: SessionCompactionTriggerSnapshot::Manual,
                source_generation,
                started_at_ms: prepared_at_ms,
                cancellable: true,
            },
            Some(cancellation.clone()),
        ) {
            Ok(guard) => guard,
            Err(_) => {
                let _ = self
                    .store
                    .finish_session_compaction(SessionHistoryCompactionFinish {
                        operation_id: request.operation_id,
                        session_id: request.session_id.clone(),
                        expected_generation: source_generation,
                        kind: SessionHistoryCompactionFinishKind::Interrupted,
                        finished_at_ms: prepared_at_ms,
                    })
                    .await;
                return Err(RuntimeError::SessionCompactionInProgress {
                    session_id: request.session_id,
                });
            }
        };
        let context = ManualCompactionContext {
            session: session.clone(),
            store: self.store.clone(),
            events: self.event_sender.clone(),
            compactor,
            operation_id: request.operation_id.clone(),
            source_generation,
            source,
            cancellation,
            guard,
        };
        let panic_store = self.store.clone();
        let panic_finish = SessionHistoryCompactionFinish {
            operation_id: request.operation_id,
            session_id: request.session_id,
            expected_generation: source_generation,
            kind: SessionHistoryCompactionFinishKind::Interrupted,
            finished_at_ms: prepared_at_ms,
        };
        let (reply, result) = oneshot::channel();
        self.tasks.spawn(
            async move {
                let outcome = run_manual_compaction(context).await;
                let _ = reply.send(outcome);
            },
            async move {
                let _ = panic_store.finish_session_compaction(panic_finish).await;
            },
        );
        drop(mutation);
        result
            .await
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "manual compaction task",
            })?
    }

    pub async fn cancel_session_compaction(
        &self,
        request: CancelSessionCompactionRequest,
    ) -> RuntimeResult<CancelSessionCompactionResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let cancellation = {
            let state = session.lock_state()?;
            state
                .active_compaction
                .as_ref()
                .filter(|active| {
                    active.snapshot.compaction_id == request.operation_id.as_str()
                        && active.snapshot.cancellable
                })
                .and_then(|active| active.cancellation.clone())
                .ok_or_else(|| RuntimeError::SessionCompactionNotFound {
                    session_id: request.session_id.clone(),
                })?
        };
        cancellation.cancel();
        Ok(CancelSessionCompactionResult {
            session_id: request.session_id,
            operation_id: request.operation_id,
        })
    }
}

async fn run_manual_compaction(
    mut context: ManualCompactionContext,
) -> RuntimeResult<CompactSessionResult> {
    let outcome = execute_manual_compaction(&mut context).await;
    if let Err(error) = &outcome {
        let _ = context
            .store
            .finish_session_compaction(finish_request(
                &context,
                SessionHistoryCompactionFinishKind::Interrupted,
            ))
            .await;
        context
            .guard
            .finish(SessionCompactionFinishedOutcome::Failed {
                code: error.to_protocol_info().code,
            });
    }
    outcome
}

async fn execute_manual_compaction(
    context: &mut ManualCompactionContext,
) -> RuntimeResult<CompactSessionResult> {
    let candidate = context
        .compactor
        .compact_manual(context.source.clone(), context.cancellation.clone())
        .await;
    match candidate {
        Ok(ManualCompactionCandidate::NoOp) => {
            finish_without_replacement(
                context,
                SessionHistoryCompactionFinishKind::NoOp,
                SessionCompactionFinishedOutcome::NoOp,
            )
            .await?;
            result(&context.session, CompactSessionOutcome::NoOp)
        }
        Err(error) if error.is_cancelled() || context.cancellation.is_cancelled() => {
            finish_without_replacement(
                context,
                SessionHistoryCompactionFinishKind::Cancelled,
                SessionCompactionFinishedOutcome::Cancelled,
            )
            .await?;
            result(&context.session, CompactSessionOutcome::Cancelled)
        }
        Err(error) => Err(error.into_runtime_error()),
        Ok(ManualCompactionCandidate::Replacement {
            conversation,
            compacted_message_count,
            retained_message_count,
        }) => {
            commit_manual_replacement(
                context,
                conversation,
                compacted_message_count,
                retained_message_count,
            )
            .await
        }
    }
}

async fn finish_without_replacement(
    context: &mut ManualCompactionContext,
    kind: SessionHistoryCompactionFinishKind,
    event_outcome: SessionCompactionFinishedOutcome,
) -> RuntimeResult<()> {
    context
        .store
        .finish_session_compaction(finish_request(context, kind))
        .await
        .map_err(|source| RuntimeError::from_store("finish session compaction", source))?;
    context.guard.finish(event_outcome);
    Ok(())
}

async fn commit_manual_replacement(
    context: &mut ManualCompactionContext,
    conversation: ConversationSnapshot,
    compacted_message_count: u64,
    retained_message_count: u64,
) -> RuntimeResult<CompactSessionResult> {
    let session = context.session.clone();
    let _mutation = session.mutation().await;
    if context.cancellation.is_cancelled() {
        finish_without_replacement(
            context,
            SessionHistoryCompactionFinishKind::Cancelled,
            SessionCompactionFinishedOutcome::Cancelled,
        )
        .await?;
        return result(&context.session, CompactSessionOutcome::Cancelled);
    }
    {
        let state = context.session.lock_state()?;
        let current = state
            .journal
            .as_ref()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "manual compaction conversation",
            })?;
        if state.body_generation != context.source_generation
            || current.has_pending()
            || current.snapshot() != context.source
            || !state.active_compaction.as_ref().is_some_and(|active| {
                active.snapshot.compaction_id == context.operation_id.as_str()
            })
        {
            return Err(RuntimeError::SnapshotStale);
        }
    }
    let committed = context
        .store
        .replace_context(ContextReplacement {
            target: ContextReplacementTarget::IdleSession {
                session_id: context.session.id().clone(),
                expected_generation: context.source_generation,
                operation_id: context.operation_id.clone(),
                compacted_message_count,
                retained_message_count,
            },
            conversation: conversation.clone(),
            changed_at_ms: super::now_ms()?,
        })
        .await
        .map_err(|source| {
            if source.kind() == StoreErrorKind::Conflict {
                RuntimeError::SnapshotStale
            } else {
                RuntimeError::from_store("commit session compaction", source)
            }
        })?;
    let projection = (|| -> RuntimeResult<()> {
        let mut state = context.session.lock_state()?;
        let journal = state
            .journal
            .as_mut()
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "manual compaction projection",
            })?;
        journal.replace_completed(conversation).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "manual compaction projection",
            }
        })?;
        state.persisted_message_count = journal.message_count();
        state.message_count = committed.product_message_count;
        state.body_generation = committed.result_generation;
        Ok(())
    })();
    if let Err(error) = projection {
        let _ = context.session.mark_faulted();
        return Err(error);
    }
    let _ = context.events.send(RuntimeEvent::ConversationCommitted {
        owner: assistant_protocol::ConversationOwner::MainSession {
            session_id: context.session.id().clone(),
        },
        generation: committed.result_generation,
    });
    context
        .guard
        .finish(SessionCompactionFinishedOutcome::Compacted {
            source_generation: committed.source_generation,
            result_generation: committed.result_generation,
        });
    result(
        &context.session,
        CompactSessionOutcome::Compacted {
            source_generation: committed.source_generation,
            result_generation: committed.result_generation,
            compacted_message_count,
            retained_message_count,
        },
    )
}

fn finish_request(
    context: &ManualCompactionContext,
    kind: SessionHistoryCompactionFinishKind,
) -> SessionHistoryCompactionFinish {
    SessionHistoryCompactionFinish {
        operation_id: context.operation_id.clone(),
        session_id: context.session.id().clone(),
        expected_generation: context.source_generation,
        kind,
        finished_at_ms: super::now_ms().unwrap_or_default(),
    }
}

fn result(
    session: &SessionController,
    outcome: CompactSessionOutcome,
) -> RuntimeResult<CompactSessionResult> {
    Ok(CompactSessionResult {
        session: session.summary()?,
        outcome,
    })
}
