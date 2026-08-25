//! 子任务在 Runtime 内部的可靠记录、索引和执行装配。

pub(crate) mod cancellation;
mod controller;
mod events;
mod input;
mod settlement;
mod tool;

pub(crate) use controller::{ParentDelegationController, ParentDelegationResources};
pub(crate) use tool::DelegateTaskTool;

/// 父 Agent 可见的稳定委派工具名；Host 恢复逻辑必须复用同一常量。
pub const DELEGATE_TASK_TOOL_NAME: &str = "delegate_task";

/// 委派继续按 General 工具名匹配权限规则，但保留审批展示需要的受限事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DelegationAuthorizationFacts {
    pub(crate) title: String,
    pub(crate) task_summary: String,
}

pub(crate) const CHILD_AGENT_INSTRUCTION_V1: &str = "<child_agent>\nYou are a non-recursive child agent executing one self-contained delegated task. Work only from the task input and the explicitly provided context. Use the available tools when useful, verify important claims, and return a concise final result to the parent agent. You cannot delegate further and you do not have access to the parent conversation.\n</child_agent>";

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, RwLock},
};

use agent_types::ConversationSnapshot;
use assistant_protocol::{ChildTaskId, RunId, SessionId};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};

use crate::{StoredChildTask, StoredConversationState, journal::InMemoryJournal};

pub(crate) struct ChildTaskRecord {
    child_task_id: ChildTaskId,
    session_id: SessionId,
    /// `InMemoryJournal` 只用该标识校验 pending exchange 的所有者；子任务拥有独立 Journal，
    /// 因此可安全复用父 Run ID，而不会与主 Conversation 或 sibling 混淆。
    journal_owner: RunId,
    mutation_gate: AsyncMutex<()>,
    state: Mutex<ChildTaskJournalState>,
}

/// 进程内全部子任务的权威结构化索引。
///
/// 正文仍只属于每个 [`ChildTaskRecord`]；Registry 不复制 Conversation。
pub(crate) struct ChildTaskRegistry {
    tasks: RwLock<BTreeMap<ChildTaskId, StoredChildTask>>,
    active: Mutex<BTreeMap<ChildTaskId, ActiveChildTask>>,
}

struct ActiveChildTask {
    session_id: SessionId,
    parent_run_id: RunId,
    cancellation: Arc<cancellation::ChildTaskCancellation>,
}

impl ChildTaskRegistry {
    pub(crate) fn recovered(tasks: BTreeMap<ChildTaskId, StoredChildTask>) -> Self {
        Self {
            tasks: RwLock::new(tasks),
            active: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn contains(
        &self,
        child_task_id: &ChildTaskId,
    ) -> Result<bool, crate::RuntimeError> {
        self.tasks
            .read()
            .map(|tasks| tasks.contains_key(child_task_id))
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })
    }

    pub(crate) fn upsert(&self, task: StoredChildTask) -> Result<(), crate::RuntimeError> {
        let mut tasks =
            self.tasks
                .write()
                .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                    component: "child task registry",
                })?;
        let Some(existing) = tasks.get_mut(&task.child_task_id) else {
            tasks.insert(task.child_task_id.clone(), task);
            return Ok(());
        };

        // Store await 与取消/结算投影可能交错。内存索引只允许 Accepted -> Running -> Terminal
        // 前进，且“曾请求取消”是粘性事实，不能被较旧快照或终态结算覆盖回 false。
        let cancel_requested = existing.cancel_requested || task.cancel_requested;
        if existing.status.is_terminal()
            || (existing.status == assistant_protocol::ChildTaskStatus::Running
                && task.status == assistant_protocol::ChildTaskStatus::Accepted)
        {
            existing.cancel_requested = cancel_requested;
            return Ok(());
        }
        *existing = task;
        existing.cancel_requested = cancel_requested;
        Ok(())
    }

    pub(crate) fn get(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> Result<Option<StoredChildTask>, crate::RuntimeError> {
        self.tasks
            .read()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })
            .map(|tasks| {
                tasks
                    .get(child_task_id)
                    .filter(|task| &task.session_id == session_id)
                    .cloned()
            })
    }

    pub(crate) fn list(
        &self,
        session_id: &SessionId,
        parent_run_id: &RunId,
    ) -> Result<Vec<StoredChildTask>, crate::RuntimeError> {
        self.tasks
            .read()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })
            .map(|tasks| {
                let mut listed = tasks
                    .values()
                    .filter(|task| {
                        &task.session_id == session_id && &task.parent_run_id == parent_run_id
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                listed.sort_by(|left, right| {
                    left.created_at_ms
                        .cmp(&right.created_at_ms)
                        .then_with(|| left.child_task_id.cmp(&right.child_task_id))
                });
                listed
            })
    }

    pub(crate) fn list_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<StoredChildTask>, crate::RuntimeError> {
        self.tasks
            .read()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })
            .map(|tasks| {
                let mut listed = tasks
                    .values()
                    .filter(|task| &task.session_id == session_id)
                    .cloned()
                    .collect::<Vec<_>>();
                listed.sort_by(|left, right| {
                    left.created_at_ms
                        .cmp(&right.created_at_ms)
                        .then_with(|| left.child_task_id.cmp(&right.child_task_id))
                });
                listed
            })
    }

    pub(crate) fn active_count_for_session(
        &self,
        session_id: &SessionId,
    ) -> Result<u64, crate::RuntimeError> {
        let count = self
            .tasks
            .read()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })?
            .values()
            .filter(|task| &task.session_id == session_id && !task.status.is_terminal())
            .count();
        Ok(u64::try_from(count).unwrap_or(u64::MAX))
    }

    pub(crate) fn activate(
        &self,
        task: &StoredChildTask,
        cancellation: Arc<cancellation::ChildTaskCancellation>,
    ) -> Result<(), crate::RuntimeError> {
        let mut active =
            self.active
                .lock()
                .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                    component: "active child task registry",
                })?;
        if active.contains_key(&task.child_task_id) {
            return Err(crate::RuntimeError::InternalStateUnavailable {
                component: "duplicate active child task",
            });
        }
        active.insert(
            task.child_task_id.clone(),
            ActiveChildTask {
                session_id: task.session_id.clone(),
                parent_run_id: task.parent_run_id.clone(),
                cancellation,
            },
        );
        Ok(())
    }

    pub(crate) fn deactivate(
        &self,
        child_task_id: &ChildTaskId,
    ) -> Result<(), crate::RuntimeError> {
        self.active
            .lock()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "active child task registry",
            })?
            .remove(child_task_id);
        Ok(())
    }

    pub(crate) fn remove_session(&self, session_id: &SessionId) -> Result<(), crate::RuntimeError> {
        self.tasks
            .write()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "child task registry",
            })?
            .retain(|_, task| &task.session_id != session_id);
        self.active
            .lock()
            .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                component: "active child task registry",
            })?
            .retain(|_, task| &task.session_id != session_id);
        Ok(())
    }

    /// M3 的进程内单独取消接缝；正式公共命令与所有权错误投影在 M4 接入。
    pub(crate) fn cancel_active(
        &self,
        session_id: &SessionId,
        parent_run_id: &RunId,
        child_task_id: &ChildTaskId,
        reason: cancellation::ChildCancellationReason,
    ) -> Result<bool, crate::RuntimeError> {
        let active =
            self.active
                .lock()
                .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                    component: "active child task registry",
                })?;
        let Some(task) = active.get(child_task_id) else {
            return Ok(false);
        };
        if &task.session_id != session_id || &task.parent_run_id != parent_run_id {
            return Ok(false);
        }
        task.cancellation.request(reason);
        Ok(true)
    }
}

pub(crate) struct ChildTaskJournalState {
    pub(crate) journal: Option<InMemoryJournal>,
    pub(crate) persisted_message_count: usize,
    pub(crate) body_generation: u64,
}

impl ChildTaskRecord {
    pub(crate) fn recovered(
        stored: &StoredChildTask,
        conversation: Option<ConversationSnapshot>,
    ) -> Result<Self, crate::RuntimeError> {
        let journal = if stored.conversation_state == StoredConversationState::Available {
            let snapshot = conversation.ok_or(crate::RuntimeError::InternalStateUnavailable {
                component: "child task conversation",
            })?;
            Some(InMemoryJournal::from_snapshot(snapshot).map_err(|_| {
                crate::RuntimeError::InternalStateUnavailable {
                    component: "child task journal",
                }
            })?)
        } else {
            None
        };
        let persisted_message_count = journal.as_ref().map_or(0, InMemoryJournal::message_count);
        if u64::try_from(persisted_message_count).ok() != Some(stored.message_count) {
            return Err(crate::RuntimeError::InternalStateUnavailable {
                component: "child task message count",
            });
        }
        Ok(Self {
            child_task_id: stored.child_task_id.clone(),
            session_id: stored.session_id.clone(),
            journal_owner: stored.parent_run_id.clone(),
            mutation_gate: AsyncMutex::new(()),
            state: Mutex::new(ChildTaskJournalState {
                journal,
                persisted_message_count,
                body_generation: stored.body_generation,
            }),
        })
    }

    pub(crate) fn id(&self) -> &ChildTaskId {
        &self.child_task_id
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn journal_owner(&self) -> &RunId {
        &self.journal_owner
    }

    pub(crate) async fn mutation(&self) -> AsyncMutexGuard<'_, ()> {
        self.mutation_gate.lock().await
    }

    pub(crate) fn lock_state(
        &self,
    ) -> Result<
        MutexGuard<'_, ChildTaskJournalState>,
        std::sync::PoisonError<MutexGuard<'_, ChildTaskJournalState>>,
    > {
        self.state.lock()
    }

    /// Store 已提交终态后，把同一批完整消息应用到内存 Journal。
    pub(crate) fn commit_terminal(
        &self,
        messages: &[agent_types::ConversationMessage],
    ) -> Result<(), crate::RuntimeError> {
        let mut state =
            self.lock_state()
                .map_err(|_| crate::RuntimeError::InternalStateUnavailable {
                    component: "child task journal",
                })?;
        let journal =
            state
                .journal
                .as_mut()
                .ok_or(crate::RuntimeError::InternalStateUnavailable {
                    component: "child task journal",
                })?;
        for message in messages {
            journal.append_completed(message.clone()).map_err(|_| {
                crate::RuntimeError::InternalStateUnavailable {
                    component: "child task terminal conversation",
                }
            })?;
        }
        state.persisted_message_count = journal.message_count();
        Ok(())
    }

    /// Store 已原子切换子正文 generation 后，同步替换进程内有效 Conversation。
    pub(crate) fn replace_conversation(
        &self,
        replacement: ConversationSnapshot,
    ) -> Result<(), crate::context_compaction::RuntimeCompactionError> {
        let mut state = self
            .lock_state()
            .map_err(|_| crate::context_compaction::RuntimeCompactionError::Projection)?;
        let journal = state
            .journal
            .as_mut()
            .ok_or(crate::context_compaction::RuntimeCompactionError::Projection)?;
        journal
            .replace_completed(replacement)
            .map_err(|_| crate::context_compaction::RuntimeCompactionError::Projection)?;
        state.persisted_message_count = journal.message_count();
        state.body_generation = state.body_generation.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use agent_model::SystemPromptSnapshot;
    use assistant_protocol::{AgentVariant, ChildTaskStatus};

    use super::*;

    #[test]
    fn registry_never_regresses_terminal_state_and_keeps_cancellation_history() {
        let child_task_id = ChildTaskId::new("ct-registry").expect("child id");
        let registry = ChildTaskRegistry::recovered(BTreeMap::new());
        let mut running = stored_task(child_task_id.clone(), ChildTaskStatus::Running);
        registry.upsert(running.clone()).expect("insert running");

        let mut terminal = running.clone();
        terminal.status = ChildTaskStatus::Completed;
        terminal.finished_at_ms = Some(2);
        registry.upsert(terminal).expect("insert terminal");

        running.cancel_requested = true;
        registry.upsert(running).expect("merge stale cancellation");
        let observed = registry
            .get(
                &SessionId::new("s-registry").expect("session id"),
                &child_task_id,
            )
            .expect("registry")
            .expect("task");
        assert_eq!(observed.status, ChildTaskStatus::Completed);
        assert!(observed.cancel_requested);
        assert_eq!(observed.finished_at_ms, Some(2));
    }

    fn stored_task(child_task_id: ChildTaskId, status: ChildTaskStatus) -> StoredChildTask {
        StoredChildTask {
            child_task_id,
            session_id: SessionId::new("s-registry").expect("session id"),
            parent_run_id: RunId::new("r-registry").expect("run id"),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("call-registry")
                .expect("call id"),
            title: "registry".to_owned(),
            system_prompt: SystemPromptSnapshot::default(),
            agent_variant: AgentVariant::Build,
            status,
            cancel_requested: false,
            body_generation: 1,
            message_count: 1,
            final_message_id: None,
            error: None,
            created_at_ms: 1,
            started_at_ms: Some(1),
            finished_at_ms: None,
            conversation_state: StoredConversationState::Available,
        }
    }
}
