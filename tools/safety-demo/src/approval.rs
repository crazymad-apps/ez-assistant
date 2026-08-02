//! Demo 私有的一次性审批协调器与 Authorizer。

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
/// 页面可以对单个 pending 调用做出的两种一次性决定。
pub(crate) enum ApprovalDecision {
    AllowOnce,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 页面展示审批卡所需的只读事实快照。
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
/// 进程内只允许一个 pending 审批的协调器。
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
            .map(|pending| pending.snapshot.clone())
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .is_some()
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
    ) -> Result<PendingApprovalSnapshot, ApprovalError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(pending) = state.pending.as_ref() else {
            return Err(ApprovalError::NotPending);
        };
        if pending.snapshot.approval_id != approval_id {
            return Err(ApprovalError::NotPending);
        }
        let mut pending = state.pending.take().ok_or(ApprovalError::NotPending)?;
        let sender = pending
            .decision
            .take()
            .ok_or(ApprovalError::AlreadyDecided)?;
        sender.send(decision).map_err(|_| ApprovalError::NoWaiter)?;
        Ok(pending.snapshot)
    }

    fn cancel_if(&self, approval_id: &str) -> Option<(String, ToolCallId)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let matches = state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.snapshot.approval_id == approval_id);
        matches.then(|| {
            let pending = state.pending.take().expect("matched pending exists");
            (pending.snapshot.run_id, pending.call_id)
        })
    }

    pub(crate) fn clear(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending = None;
    }
}

/// 把 Core 未决授权转换成一次性页面审批等待。
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
                Ok(created) => created,
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
            guard.disarm();
            match decision {
                Some(ApprovalDecision::AllowOnce) => {
                    self.audit.record_approval_decision(
                        &self.run_id,
                        invocation.call_id(),
                        AuditDecision::Allow,
                    );
                    (self.notify)();
                    ToolAuthorization::Allow
                }
                Some(ApprovalDecision::Deny) => {
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
                    self.audit
                        .record_approval_cancelled(&self.run_id, invocation.call_id());
                    (self.notify)();
                    ToolAuthorization::Deny {
                        reason: "approval was cancelled".to_owned(),
                    }
                }
            }
        })
    }
}

/// Core 的取消 race 会直接 drop Authorizer future；Drop 中同步清理内存 pending 卡片。
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
        let coordinator = self.coordinator.clone();
        let audit = self.audit.clone();
        let notify = self.notify.clone();
        if let Some((run_id, call_id)) = coordinator.cancel_if(&approval_id) {
            audit.record_approval_cancelled(&run_id, &call_id);
            notify();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_repeated_decision_is_rejected() {
        let coordinator = ApprovalCoordinator::default();
        assert_eq!(
            coordinator.decide("approval-1", ApprovalDecision::AllowOnce),
            Err(ApprovalError::NotPending)
        );
    }
}
