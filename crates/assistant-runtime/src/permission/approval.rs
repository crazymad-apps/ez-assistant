//! Runtime 内存 pending approval Registry 与 Core 授权等待适配。
//!
//! 阅读顺序：
//! 1. [`ApprovalRegistry`] 保存尚未形成用户决策的审批，并用 oneshot 把决策交还给
//!    正在等待的 Core 授权调用；
//! 2. `begin_resolution` 先把审批从 `Pending` 占用为 `Resolving`，避免两个客户端
//!    同时处理同一审批；
//! 3. [`RuntimeApprovalResolver`] 负责把 Core 的一次 Ask 挂起到用户决策或 Run 取消；
//! 4. `rule_for_approval` 只为持久授权生成当前调用的精确规则，不扩大授权范围。
//!
//! Registry 是进程内协调状态，不是恢复来源。Runtime 重启后，未决审批随旧 Run 一起
//! 结算为中断，不尝试恢复或继续执行可能带副作用的工具。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use agent_tools::{
    FileAuthorizationFacts, FileBatchAuthorizationFacts, FileOperation, GeneralAuthorizationFacts,
    ResolvedToolBatch, ResolvedToolInvocation, ShellAuthorizationFacts, ShellProcessMode,
};
use assistant_protocol::{
    AgentVariant, ApprovalDecision, ApprovalId, ApprovalMode, ApprovalSnapshot, ApprovalStatus,
    ChildTaskId, RunId, RuntimeEvent, SessionId, ToolApprovalSubject, WorkspaceId,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{
    CommandMatch, FilePermissionMatcher, GeneralPermissionMatcher, PathMatch, PermissionEffect,
    PermissionFileOperation, PermissionMatcher, PermissionProcessMode, PermissionRule,
    ShellPermissionMatcher,
    authorizer::{ApprovalFuture, PermissionApprovalResolver},
};
use crate::{
    RuntimeError, RuntimeResult,
    delegation::{DELEGATE_TASK_TOOL_NAME, DelegationAuthorizationFacts},
    id,
    observation::ObservationCoordinator,
};

struct PendingApproval {
    snapshot: ApprovalSnapshot,
    invocation: ResolvedToolInvocation,
    // sender 只允许被成功的决策路径取走一次。删除 Registry 项本身不代表 Core 已收到决策。
    sender: Option<oneshot::Sender<ApprovalSignal>>,
}

enum ApprovalSignal {
    User(ApprovalDecision),
    Rule,
}

struct ApprovalContext {
    session_id: SessionId,
    run_id: RunId,
    child_task_id: Option<ChildTaskId>,
    variant: AgentVariant,
    approval_mode: ApprovalMode,
    workspace_id: Option<WorkspaceId>,
}

/// 当前进程内所有未决审批的权威 Registry。
///
/// `std::sync::Mutex` 只保护无 await 的短小 map 操作；审批等待发生在锁外，因此一个
/// Session 等待用户输入时不会阻塞其他 Session 查询或处理审批。
pub(crate) struct ApprovalRegistry {
    entries: Mutex<BTreeMap<ApprovalId, PendingApproval>>,
    revisions: Mutex<BTreeMap<SessionId, u64>>,
}

impl ApprovalRegistry {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            revisions: Mutex::new(BTreeMap::new()),
        }
    }

    fn register(
        &self,
        context: ApprovalContext,
        invocation: &ResolvedToolInvocation,
    ) -> RuntimeResult<(ApprovalSnapshot, oneshot::Receiver<ApprovalSignal>)> {
        let approval_id = ApprovalId::new(id::generate("a").map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "approval id random source",
            }
        })?)
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "approval id generator",
        })?;
        let subject = subject(invocation).ok_or(RuntimeError::InternalStateUnavailable {
            component: "approval subject",
        })?;
        let exact_rule_preview = exact_rule_subject(&subject);
        let mut available_decisions =
            vec![ApprovalDecision::AllowOnce, ApprovalDecision::AllowSession];
        if context.workspace_id.is_some() {
            available_decisions.push(ApprovalDecision::AllowWorkspace);
        }
        available_decisions.push(ApprovalDecision::Deny);
        let snapshot = ApprovalSnapshot {
            approval_id: approval_id.clone(),
            session_id: context.session_id,
            run_id: context.run_id,
            child_task_id: context.child_task_id,
            call_id: assistant_protocol::ToolCallId::new(invocation.call_id().as_str()).map_err(
                |_| RuntimeError::InternalStateUnavailable {
                    component: "approval call id",
                },
            )?,
            variant: context.variant,
            approval_mode: context.approval_mode,
            subject: subject.clone(),
            available_decisions,
            exact_rule_preview,
            status: ApprovalStatus::Pending,
            created_at_ms: crate::runtime::now_ms()?,
        };
        let (sender, receiver) = oneshot::channel();
        self.entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?
            .insert(
                approval_id,
                PendingApproval {
                    snapshot: snapshot.clone(),
                    invocation: invocation.clone(),
                    sender: Some(sender),
                },
            );
        self.bump_revision(&snapshot.session_id);
        Ok((snapshot, receiver))
    }

    pub(crate) fn list(&self, session_id: &SessionId) -> RuntimeResult<Vec<ApprovalSnapshot>> {
        let mut approvals = self
            .entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?
            .values()
            .filter(|entry| &entry.snapshot.session_id == session_id)
            .map(|entry| entry.snapshot.clone())
            .collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.approval_id.cmp(&right.approval_id))
        });
        Ok(approvals)
    }

    pub(crate) fn head_with_invocation(
        &self,
        session_id: &SessionId,
    ) -> RuntimeResult<Option<(ApprovalSnapshot, ResolvedToolInvocation)>> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?;
        Ok(entries
            .values()
            .filter(|entry| &entry.snapshot.session_id == session_id)
            .min_by(|left, right| {
                left.snapshot
                    .created_at_ms
                    .cmp(&right.snapshot.created_at_ms)
                    .then_with(|| left.snapshot.approval_id.cmp(&right.snapshot.approval_id))
            })
            .map(|entry| (entry.snapshot.clone(), entry.invocation.clone())))
    }

    pub(crate) fn begin_resolution(
        &self,
        session_id: &SessionId,
        approval_id: &ApprovalId,
    ) -> RuntimeResult<ApprovalSnapshot> {
        let mut entries =
            self.entries
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "approval registry",
                })?;
        let requested = entries
            .get(approval_id)
            .filter(|entry| &entry.snapshot.session_id == session_id)
            .ok_or_else(|| RuntimeError::ApprovalNotFound {
                approval_id: approval_id.clone(),
            })?;
        if requested.snapshot.status != ApprovalStatus::Pending {
            return Err(RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            });
        }
        let head = entries
            .values()
            .filter(|entry| &entry.snapshot.session_id == session_id)
            .min_by(|left, right| {
                left.snapshot
                    .created_at_ms
                    .cmp(&right.snapshot.created_at_ms)
                    .then_with(|| left.snapshot.approval_id.cmp(&right.snapshot.approval_id))
            })
            .map(|entry| entry.snapshot.approval_id.clone());
        if head.as_ref() != Some(approval_id) {
            return Err(RuntimeError::ApprovalNotHead {
                approval_id: approval_id.clone(),
            });
        }
        let entry = entries
            .get_mut(approval_id)
            .expect("approval existence checked above");
        // Pending -> Resolving 是审批的并发占用点。后到的重复决策必须失败，不能覆盖
        // 正在执行的权限文件写入或把另一个 decision 发送给 Core。
        debug_assert_eq!(entry.snapshot.status, ApprovalStatus::Pending);
        entry.snapshot.status = ApprovalStatus::Resolving;
        let snapshot = entry.snapshot.clone();
        drop(entries);
        self.bump_revision(session_id);
        Ok(snapshot)
    }

    pub(crate) fn restore_pending(&self, approval_id: &ApprovalId) -> RuntimeResult<()> {
        let mut entries =
            self.entries
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "approval registry",
                })?;
        let entry = entries
            .get_mut(approval_id)
            .ok_or_else(|| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?;
        // 决策请求只取得了临时处理权；在持久化完成前失败或被取消时，审批仍然有效，
        // 因而必须回到 Pending 供客户端重试，而不是误报为已经解决。
        entry.snapshot.status = ApprovalStatus::Pending;
        let session_id = entry.snapshot.session_id.clone();
        drop(entries);
        self.bump_revision(&session_id);
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        approval_id: &ApprovalId,
        decision: ApprovalDecision,
    ) -> RuntimeResult<ApprovalSnapshot> {
        let mut entry = self
            .entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?
            .remove(approval_id)
            .ok_or_else(|| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?;
        let snapshot = entry.snapshot.clone();
        // 先从 Registry 移除，再发送决策。发送成功后该 approval 不应再被查询或二次处理；
        // 若 receiver 已消失，则返回 Expired，调用方也不会发布 ApprovalResolved。
        entry
            .sender
            .take()
            .ok_or_else(|| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?
            .send(ApprovalSignal::User(decision))
            .map_err(|_| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?;
        self.bump_revision(&snapshot.session_id);
        Ok(snapshot)
    }

    pub(crate) fn resolve_by_rule(
        &self,
        approval_id: &ApprovalId,
    ) -> RuntimeResult<ApprovalSnapshot> {
        let mut entry = self
            .entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?
            .remove(approval_id)
            .ok_or_else(|| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?;
        let snapshot = entry.snapshot.clone();
        entry
            .sender
            .take()
            .ok_or_else(|| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?
            .send(ApprovalSignal::Rule)
            .map_err(|_| RuntimeError::ApprovalExpired {
                approval_id: approval_id.clone(),
            })?;
        self.bump_revision(&snapshot.session_id);
        Ok(snapshot)
    }

    fn cancel(&self, approval_id: &ApprovalId) -> RuntimeResult<Option<ApprovalSnapshot>> {
        let removed = self
            .entries
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval registry",
            })?
            .remove(approval_id)
            .map(|entry| entry.snapshot);
        if let Some(approval) = &removed {
            self.bump_revision(&approval.session_id);
        }
        Ok(removed)
    }

    /// 原子占用一个 Run 中仍处于 Pending 的审批，阻止并发用户决策越过取消流程。
    pub(crate) fn begin_run_cancellation(
        &self,
        session_id: &SessionId,
        run_id: &RunId,
    ) -> RuntimeResult<Vec<ApprovalSnapshot>> {
        let mut entries =
            self.entries
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "approval registry",
                })?;
        let mut approvals = Vec::new();
        for entry in entries.values_mut() {
            if &entry.snapshot.session_id == session_id
                && &entry.snapshot.run_id == run_id
                && entry.snapshot.status == ApprovalStatus::Pending
            {
                entry.snapshot.status = ApprovalStatus::Resolving;
                approvals.push(entry.snapshot.clone());
            }
        }
        if !approvals.is_empty() {
            drop(entries);
            self.bump_revision(session_id);
        }
        Ok(approvals)
    }

    /// 取消审计失败时把尚未移除的占用项还原为可处理状态。
    pub(crate) fn abort_run_cancellation(
        &self,
        approvals: &[ApprovalSnapshot],
    ) -> RuntimeResult<()> {
        let mut entries =
            self.entries
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "approval registry",
                })?;
        for approval in approvals {
            if let Some(entry) = entries.get_mut(&approval.approval_id)
                && entry.snapshot.status == ApprovalStatus::Resolving
            {
                entry.snapshot.status = ApprovalStatus::Pending;
            }
        }
        drop(entries);
        if let Some(approval) = approvals.first() {
            self.bump_revision(&approval.session_id);
        }
        Ok(())
    }

    /// 审计已经可靠写入后移除占用项并关闭等待中的 receiver。
    pub(crate) fn finish_run_cancellation(
        &self,
        approvals: &[ApprovalSnapshot],
    ) -> RuntimeResult<Vec<ApprovalSnapshot>> {
        let mut entries =
            self.entries
                .lock()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "approval registry",
                })?;
        let mut removed = Vec::new();
        for approval in approvals {
            let matches = entries.get(&approval.approval_id).is_some_and(|entry| {
                entry.snapshot.status == ApprovalStatus::Resolving
                    && entry.snapshot.session_id == approval.session_id
                    && entry.snapshot.run_id == approval.run_id
            });
            if matches && let Some(entry) = entries.remove(&approval.approval_id) {
                removed.push(entry.snapshot);
            }
        }
        drop(entries);
        if let Some(approval) = removed.first() {
            self.bump_revision(&approval.session_id);
        }
        Ok(removed)
    }

    pub(crate) fn revision(&self, session_id: &SessionId) -> RuntimeResult<u64> {
        Ok(*self
            .revisions
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "approval queue revisions",
            })?
            .get(session_id)
            .unwrap_or(&0))
    }

    fn bump_revision(&self, session_id: &SessionId) {
        let Ok(mut revisions) = self.revisions.lock() else {
            return;
        };
        let revision = revisions.entry(session_id.clone()).or_default();
        *revision = revision.saturating_add(1);
    }
}

pub(crate) struct RuntimeApprovalResolver {
    pub(crate) registry: Arc<ApprovalRegistry>,
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) child_task_id: Option<ChildTaskId>,
    pub(crate) variant: AgentVariant,
    pub(crate) approval_mode: ApprovalMode,
    pub(crate) workspace_id: Option<WorkspaceId>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) events: ObservationCoordinator,
}

/// 保证等待 Future 被上层直接丢弃时也不会把 approval 遗留在 Registry 中。
///
/// 这里不能只依赖 `CancellationToken` 分支：Runtime shutdown、supervisor abort 或未来调用方
/// 提前丢弃授权 Future 时，`select!` 可能没有机会完成。Drop 是 Registry 清理的最后防线。
struct PendingApprovalGuard {
    registry: Arc<ApprovalRegistry>,
    approval_id: ApprovalId,
    events: ObservationCoordinator,
}

impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        if let Ok(Some(snapshot)) = self.registry.cancel(&self.approval_id) {
            let _ = self.events.send(RuntimeEvent::ApprovalCancelled {
                session_id: snapshot.session_id,
                run_id: snapshot.run_id,
                child_task_id: snapshot.child_task_id,
                approval_id: self.approval_id.clone(),
            });
        }
    }
}

impl PermissionApprovalResolver for RuntimeApprovalResolver {
    fn resolve<'a>(
        &'a self,
        invocation: &'a ResolvedToolInvocation,
        _batch: &'a ResolvedToolBatch,
    ) -> ApprovalFuture<'a> {
        Box::pin(async move {
            let (snapshot, receiver) = match self.registry.register(
                ApprovalContext {
                    session_id: self.session_id.clone(),
                    run_id: self.run_id.clone(),
                    child_task_id: self.child_task_id.clone(),
                    variant: self.variant,
                    approval_mode: self.approval_mode,
                    workspace_id: self.workspace_id.clone(),
                },
                invocation,
            ) {
                Ok(value) => value,
                Err(_) => {
                    return super::authorizer::ApprovalResolution::denied(
                        "approval could not be created",
                    );
                }
            };
            let approval_id = snapshot.approval_id.clone();
            // Guard 的生命周期覆盖整个等待区间。正常 resolve 会先从 Registry 删除记录，
            // 因此随后 Drop 是空操作；取消/abort 则由 Drop 回收并发布取消事件。
            let _cleanup = PendingApprovalGuard {
                registry: self.registry.clone(),
                approval_id: approval_id.clone(),
                events: self.events.clone(),
            };
            let _ = self.events.send(RuntimeEvent::ApprovalRequested {
                approval: Box::new(snapshot),
            });
            tokio::select! {
                decision = receiver => match decision {
                    Ok(ApprovalSignal::User(decision @ (ApprovalDecision::AllowOnce | ApprovalDecision::AllowSession | ApprovalDecision::AllowWorkspace))) => {
                        super::authorizer::ApprovalResolution::allowed(approval_id, decision)
                    }
                    Ok(ApprovalSignal::User(decision @ ApprovalDecision::Deny)) => {
                        super::authorizer::ApprovalResolution::user_denied(approval_id, decision)
                    }
                    Ok(ApprovalSignal::Rule) => {
                        super::authorizer::ApprovalResolution::allowed_by_rule(approval_id)
                    }
                    Err(_) => super::authorizer::ApprovalResolution::cancelled(
                        approval_id,
                        "approval expired before a decision was applied",
                    ),
                },
                _ = self.cancellation.cancelled() => super::authorizer::ApprovalResolution::cancelled(
                    approval_id,
                    "tool call approval was cancelled",
                )
            }
        })
    }
}

pub(crate) fn rules_for_approval(
    snapshot: &ApprovalSnapshot,
) -> RuntimeResult<Vec<PermissionRule>> {
    // “本 Session/Workspace 允许”只改变规则保存位置，不扩大匹配器：文件固定到本次
    // operation + path，Shell 固定到完整 command + cwd + process mode，并保留当前变体。
    let matchers = match &snapshot.exact_rule_preview {
        ToolApprovalSubject::General { tool_name } => {
            vec![PermissionMatcher::General(GeneralPermissionMatcher {
                tool_name: tool_name.clone(),
            })]
        }
        ToolApprovalSubject::Delegation { tool_name, .. } => {
            vec![PermissionMatcher::General(GeneralPermissionMatcher {
                tool_name: tool_name.clone(),
            })]
        }
        ToolApprovalSubject::File {
            operation, path, ..
        } => vec![PermissionMatcher::File(FilePermissionMatcher {
            operation: parse_file_operation(operation)?,
            path: path.clone(),
            path_match: PathMatch::Exact,
        })],
        ToolApprovalSubject::Files {
            operation, paths, ..
        } => paths
            .iter()
            .map(|path| {
                Ok(PermissionMatcher::File(FilePermissionMatcher {
                    operation: parse_file_operation(operation)?,
                    path: path.clone(),
                    path_match: PathMatch::Exact,
                }))
            })
            .collect::<RuntimeResult<Vec<_>>>()?,
        ToolApprovalSubject::Shell {
            command,
            working_directory,
            process_mode,
            ..
        } => vec![PermissionMatcher::Shell(ShellPermissionMatcher {
            command: command.clone(),
            command_match: CommandMatch::Exact,
            working_directory: working_directory.clone(),
            process_mode: if process_mode == "managed" {
                PermissionProcessMode::Managed
            } else {
                PermissionProcessMode::Detached
            },
        })],
    };
    matchers
        .into_iter()
        .map(|matcher| {
            Ok(PermissionRule {
                id: id::generate("rule").map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "permission rule id",
                })?,
                effect: PermissionEffect::Allow,
                variants: vec![snapshot.variant],
                matcher,
            })
        })
        .collect()
}

fn subject(invocation: &ResolvedToolInvocation) -> Option<ToolApprovalSubject> {
    if let Some(facts) = invocation.facts::<FileAuthorizationFacts>() {
        return Some(ToolApprovalSubject::File {
            tool_name: invocation.tool_name().as_str().to_owned(),
            operation: file_operation(facts.operation).to_owned(),
            path: facts.path.as_path().to_string_lossy().into_owned(),
        });
    }
    if let Some(facts) = invocation.facts::<FileBatchAuthorizationFacts>() {
        return Some(ToolApprovalSubject::Files {
            tool_name: invocation.tool_name().as_str().to_owned(),
            operation: file_operation(facts.operation).to_owned(),
            paths: facts
                .paths
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect(),
        });
    }
    if let Some(facts) = invocation.facts::<ShellAuthorizationFacts>() {
        return Some(ToolApprovalSubject::Shell {
            tool_name: invocation.tool_name().as_str().to_owned(),
            command: facts.command.clone(),
            working_directory: facts.workdir.as_path().to_string_lossy().into_owned(),
            timeout_ms: u64::try_from(facts.timeout.as_millis()).unwrap_or(u64::MAX),
            process_mode: match facts.process_mode {
                ShellProcessMode::Managed => "managed",
                ShellProcessMode::Detached => "detached",
            }
            .to_owned(),
        });
    }
    if let Some(facts) = invocation.facts::<DelegationAuthorizationFacts>() {
        return Some(ToolApprovalSubject::Delegation {
            tool_name: DELEGATE_TASK_TOOL_NAME.to_owned(),
            title: facts.title.clone(),
            task_summary: facts.task_summary.clone(),
        });
    }
    invocation
        .facts::<GeneralAuthorizationFacts>()
        .map(|facts| ToolApprovalSubject::General {
            tool_name: facts.tool_name.as_str().to_owned(),
        })
}

fn exact_rule_subject(subject: &ToolApprovalSubject) -> ToolApprovalSubject {
    match subject {
        ToolApprovalSubject::Delegation { tool_name, .. } => ToolApprovalSubject::General {
            tool_name: tool_name.clone(),
        },
        other => other.clone(),
    }
}

fn file_operation(operation: FileOperation) -> &'static str {
    match operation {
        FileOperation::Read => "read",
        FileOperation::List => "list",
        FileOperation::Find => "find",
        FileOperation::Search => "search",
        FileOperation::Write => "write",
        FileOperation::Edit => "edit",
        FileOperation::Delete => "delete",
    }
}
fn parse_file_operation(value: &str) -> RuntimeResult<PermissionFileOperation> {
    match value {
        "read" => Ok(PermissionFileOperation::Read),
        "list" => Ok(PermissionFileOperation::List),
        "find" => Ok(PermissionFileOperation::Find),
        "search" => Ok(PermissionFileOperation::Search),
        "write" => Ok(PermissionFileOperation::Write),
        "edit" => Ok(PermissionFileOperation::Edit),
        "delete" => Ok(PermissionFileOperation::Delete),
        _ => Err(RuntimeError::InternalStateUnavailable {
            component: "approval file operation",
        }),
    }
}
