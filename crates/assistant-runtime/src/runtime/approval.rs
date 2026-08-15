//! Pending approval 查询与用户决策命令。
//!
//! 该模块是“客户端决策”到“Core 继续执行”的线性化边界。持久授权必须先完成权限文件
//! CAS 写入和内存 Registry 更新，之后才能唤醒 Core；否则工具可能已经执行，而规则尚未
//! 成为可恢复事实。

use assistant_protocol::{
    ApprovalDecision, ApprovalId, DecideApprovalRequest, DecideApprovalResult,
    ListPendingApprovalsRequest, ListPendingApprovalsResult, RejectApprovalAndStopRunRequest,
    RejectApprovalAndStopRunResult, RuntimeEvent,
};

use super::AssistantRuntime;
use crate::{PermissionFileScope, RuntimeError, RuntimeResult, permission::rule_for_approval};

/// HTTP 请求被中断或任务 panic 时，把未完成的 `Resolving` 恢复为可重试状态。
///
/// `Resolving` 只是并发占用态，不是用户决策已经生效的事实。只有 `resolve` 成功后才能
/// disarm；权限文件无效、CAS 失败和请求 Future 被丢弃都应允许客户端用同一审批重试。
struct ResolutionGuard<'a> {
    runtime: &'a AssistantRuntime,
    approval_id: ApprovalId,
    armed: bool,
}

impl ResolutionGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResolutionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .runtime
                .approval_registry
                .restore_pending(&self.approval_id);
        }
    }
}

impl AssistantRuntime {
    /// 拒绝队首审批并立即请求停止其所属父 Run。
    pub async fn reject_approval_and_stop_run(
        &self,
        request: RejectApprovalAndStopRunRequest,
    ) -> RuntimeResult<RejectApprovalAndStopRunResult> {
        if self.approval_registry.revision(&request.session_id)? != request.expected_queue_revision
        {
            return Err(RuntimeError::QueueConflict);
        }
        let approval = self
            .approval_registry
            .list(&request.session_id)?
            .into_iter()
            .find(|approval| approval.approval_id == request.approval_id)
            .ok_or_else(|| RuntimeError::ApprovalNotFound {
                approval_id: request.approval_id.clone(),
            })?;
        self.decide_approval(DecideApprovalRequest {
            session_id: request.session_id.clone(),
            approval_id: request.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await?;
        let run = self
            .cancel_run(assistant_protocol::CancelRunRequest {
                session_id: request.session_id.clone(),
                run_id: approval.run_id,
            })
            .await?
            .run;
        Ok(RejectApprovalAndStopRunResult {
            run,
            approvals: self.approval_queue(&request.session_id)?,
        })
    }

    /// 在传播 Run cancellation 前原子占用并移除仍在等待的内存审批。
    pub(super) async fn cancel_run_approvals(
        &self,
        session_id: &assistant_protocol::SessionId,
        run_id: &assistant_protocol::RunId,
    ) -> RuntimeResult<()> {
        let approvals = self
            .approval_registry
            .begin_run_cancellation(session_id, run_id)?;
        if approvals.is_empty() {
            return Ok(());
        }
        let removed = match self.approval_registry.finish_run_cancellation(&approvals) {
            Ok(removed) => removed,
            Err(error) => {
                self.approval_registry.abort_run_cancellation(&approvals)?;
                return Err(error);
            }
        };
        for approval in removed {
            let _ = self.event_sender.send(RuntimeEvent::ApprovalCancelled {
                session_id: approval.session_id,
                run_id: approval.run_id,
                child_task_id: approval.child_task_id,
                approval_id: approval.approval_id,
            });
        }
        Ok(())
    }

    pub fn list_pending_approvals(
        &self,
        request: ListPendingApprovalsRequest,
    ) -> RuntimeResult<ListPendingApprovalsResult> {
        self.session(&request.session_id)?;
        Ok(ListPendingApprovalsResult {
            approvals: self.approval_registry.list(&request.session_id)?,
        })
    }

    pub async fn decide_approval(
        &self,
        request: DecideApprovalRequest,
    ) -> RuntimeResult<DecideApprovalResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let session = self.session(&request.session_id)?;
        let snapshot = self
            .approval_registry
            .begin_resolution(&request.session_id, &request.approval_id)?;
        let mut resolution = ResolutionGuard {
            runtime: self,
            approval_id: request.approval_id.clone(),
            armed: true,
        };
        if !snapshot.available_decisions.contains(&request.decision) {
            return Err(RuntimeError::PermissionScopeUnavailable);
        }

        let persistent_scope = match request.decision {
            ApprovalDecision::AllowSession => {
                Some(PermissionFileScope::Session(request.session_id.clone()))
            }
            ApprovalDecision::AllowWorkspace => Some(PermissionFileScope::Workspace(
                session
                    .environment()
                    .workspace_id
                    .clone()
                    .ok_or(RuntimeError::PermissionScopeUnavailable)?,
            )),
            ApprovalDecision::AllowOnce | ApprovalDecision::Deny => None,
        };
        if let Some(scope) = persistent_scope {
            // 不提前唤醒 Core：append_allow_rule 同时保证磁盘事实和运行时快照就绪。
            // 若这里失败，ResolutionGuard 会把审批恢复为 Pending，工具保持未执行。
            let result = match rule_for_approval(&snapshot) {
                Ok(rule) => {
                    self.permission_coordinator
                        .append_allow_rule(scope, rule)
                        .await
                }
                Err(error) => Err(error),
            };
            result?;
        }

        // AllowOnce/Deny 无持久化步骤；持久 Allow 则只会在上面的写入成功后到达这里。
        // resolve 是唯一向等待中的 Core 发送最终 decision 的位置。
        let resolved = self
            .approval_registry
            .resolve(&request.approval_id, request.decision)?;
        resolution.disarm();
        let _ = self.event_sender.send(RuntimeEvent::ApprovalResolved {
            session_id: resolved.session_id,
            run_id: resolved.run_id,
            child_task_id: resolved.child_task_id,
            approval_id: request.approval_id.clone(),
            decision: request.decision,
        });
        Ok(DecideApprovalResult {
            approval_id: request.approval_id,
            decision: request.decision,
        })
    }
}
