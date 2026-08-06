//! Core Demo 私有的一次性审批协调器。

use std::sync::{Arc, Mutex};

use agent_core::{AuthorizationFuture, ToolAuthorization, ToolAuthorizer};
use agent_tools::{ResolvedToolBatch, ResolvedToolInvocation};
use agent_types::ToolCallId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditDecision, AuditFacts, DemoAudit};

pub(crate) type StateChangeNotifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalDecision {
    AllowOnce,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct PendingApprovalSnapshot {
    pub approval_id: String,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub facts: AuditFacts,
}

struct PendingApproval {
    snapshot: PendingApprovalSnapshot,
    call_id: ToolCallId,
    decision: Option<oneshot::Sender<ApprovalDecision>>,
}

#[derive(Default)]
struct ApprovalState {
    pending: Option<PendingApproval>,
    next_id: u64,
}

#[derive(Clone, Default)]
pub(crate) struct ApprovalCoordinator {
    state: Arc<Mutex<ApprovalState>>,
}

impl ApprovalCoordinator {
    pub(crate) fn snapshot(&self) -> Option<PendingApprovalSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .as_ref()
            .filter(|pending| pending.decision.is_some())
            .map(|pending| pending.snapshot.clone())
    }

    fn create(
        &self,
        run_id: &str,
        invocation: &ResolvedToolInvocation,
    ) -> Result<(PendingApprovalSnapshot, oneshot::Receiver<ApprovalDecision>), ApprovalError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pending.is_some() {
            return Err(ApprovalError::Busy);
        }
        state.next_id = state.next_id.saturating_add(1);
        let snapshot = PendingApprovalSnapshot {
            approval_id: format!("approval-{}", state.next_id),
            run_id: run_id.to_owned(),
            call_id: invocation.call_id().to_string(),
            tool_name: invocation.tool_name().as_str().to_owned(),
            facts: AuditFacts::from_invocation(invocation),
        };
        let (sender, receiver) = oneshot::channel();
        state.pending = Some(PendingApproval {
            snapshot: snapshot.clone(),
            call_id: invocation.call_id().clone(),
            decision: Some(sender),
        });
        Ok((snapshot, receiver))
    }

    pub(crate) fn decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = state.pending.as_mut().ok_or(ApprovalError::NotPending)?;
        if pending.snapshot.approval_id != approval_id {
            return Err(ApprovalError::NotPending);
        }
        pending
            .decision
            .take()
            .ok_or(ApprovalError::AlreadyDecided)?
            .send(decision)
            .map_err(|_| ApprovalError::NoWaiter)
    }

    fn finish_if(&self, approval_id: &str) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.snapshot.approval_id == approval_id)
        {
            state.pending = None;
            true
        } else {
            false
        }
    }

    fn cancel_if(&self, approval_id: &str) -> Option<(String, ToolCallId)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.snapshot.approval_id == approval_id)
        {
            let pending = state.pending.take().expect("matched pending exists");
            Some((pending.snapshot.run_id, pending.call_id))
        } else {
            None
        }
    }
}

pub(crate) struct DemoApprovalAuthorizer {
    run_id: String,
    coordinator: ApprovalCoordinator,
    cancellation: CancellationToken,
    audit: DemoAudit,
    notify: StateChangeNotifier,
}

impl DemoApprovalAuthorizer {
    pub(crate) fn new(
        run_id: String,
        coordinator: ApprovalCoordinator,
        cancellation: CancellationToken,
        audit: DemoAudit,
        notify: StateChangeNotifier,
    ) -> Self {
        Self {
            run_id,
            coordinator,
            cancellation,
            audit,
            notify,
        }
    }
}

impl ToolAuthorizer for DemoApprovalAuthorizer {
    fn authorize<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            let (pending, receiver) = match self.coordinator.create(&self.run_id, invocation) {
                Ok(value) => value,
                Err(error) => {
                    return ToolAuthorization::Deny {
                        reason: error.to_string(),
                    };
                }
            };
            self.audit
                .record_approval_request(&self.run_id, invocation, &pending.approval_id);
            (self.notify)();
            let mut guard = PendingApprovalGuard {
                approval_id: Some(pending.approval_id.clone()),
                coordinator: self.coordinator.clone(),
                audit: self.audit.clone(),
                notify: self.notify.clone(),
            };
            let decision = tokio::select! {
                biased;
                () = self.cancellation.cancelled() => None,
                decision = receiver => decision.ok(),
            };
            match decision {
                Some(ApprovalDecision::AllowOnce)
                    if self.coordinator.finish_if(&pending.approval_id) =>
                {
                    guard.disarm();
                    self.audit.record_approval_decision(
                        &self.run_id,
                        invocation.call_id(),
                        AuditDecision::Allow,
                    );
                    (self.notify)();
                    ToolAuthorization::Allow
                }
                Some(ApprovalDecision::Deny)
                    if self.coordinator.finish_if(&pending.approval_id) =>
                {
                    guard.disarm();
                    self.audit.record_approval_decision(
                        &self.run_id,
                        invocation.call_id(),
                        AuditDecision::Deny,
                    );
                    (self.notify)();
                    ToolAuthorization::Deny {
                        reason: "user denied this one-time approval".to_owned(),
                    }
                }
                None => {
                    let _ = self.coordinator.cancel_if(&pending.approval_id);
                    guard.disarm();
                    self.audit
                        .record_approval_cancelled(&self.run_id, invocation.call_id());
                    (self.notify)();
                    ToolAuthorization::Deny {
                        reason: "approval was cancelled".to_owned(),
                    }
                }
                Some(ApprovalDecision::AllowOnce | ApprovalDecision::Deny) => {
                    guard.disarm();
                    ToolAuthorization::Deny {
                        reason: "approval was cancelled before the decision settled".to_owned(),
                    }
                }
            }
        })
    }
}

struct PendingApprovalGuard {
    approval_id: Option<String>,
    coordinator: ApprovalCoordinator,
    audit: DemoAudit,
    notify: StateChangeNotifier,
}

impl PendingApprovalGuard {
    fn disarm(&mut self) {
        self.approval_id = None;
    }
}

impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        let Some(approval_id) = self.approval_id.take() else {
            return;
        };
        if let Some((run_id, call_id)) = self.coordinator.cancel_if(&approval_id) {
            self.audit.record_approval_cancelled(&run_id, &call_id);
            (self.notify)();
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum ApprovalError {
    #[error("another approval is already pending")]
    Busy,
    #[error("approval is no longer pending")]
    NotPending,
    #[error("approval was already decided")]
    AlreadyDecided,
    #[error("approval waiter is no longer active")]
    NoWaiter,
}
