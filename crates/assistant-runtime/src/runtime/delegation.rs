//! 子任务公共查询、取消与规范正文派生投影。

use agent_types::{AssistantPart, ConversationMessage};
use assistant_protocol::{
    CancelChildTaskRequest, CancelChildTaskResult, ChildTaskSnapshot, GetChildTaskRequest,
    GetChildTaskResult, GetRunRequest, ListChildTasksRequest, ListChildTasksResult,
};

use super::AssistantRuntime;
use crate::{RuntimeError, RuntimeResult, StoredChildTask, StoredChildTaskHistory};

impl AssistantRuntime {
    pub(super) async fn child_task_conversation_snapshot(
        &self,
        session_id: &assistant_protocol::SessionId,
        child_task_id: &assistant_protocol::ChildTaskId,
    ) -> RuntimeResult<agent_types::ConversationSnapshot> {
        self.session(session_id).await?;
        self.product_child_task(session_id, child_task_id)
            .await?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: session_id.clone(),
                child_task_id: child_task_id.clone(),
            })?;
        self.store
            .load_child_conversation(session_id, child_task_id)
            .await
            .map_err(|source| RuntimeError::from_store("load child task conversation", source))
    }

    pub async fn list_child_tasks(
        &self,
        request: ListChildTasksRequest,
    ) -> RuntimeResult<ListChildTasksResult> {
        let tasks = self.product_child_tasks(&request.session_id).await?;
        // 继承父 Run 不属于可控制的 RunSnapshot；其文件目录仍证明只读查询归属。
        if !tasks
            .iter()
            .any(|task| task.parent_run_id == request.parent_run_id)
        {
            let history = self
                .store
                .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                    session_id: request.session_id.clone(),
                })
                .await
                .map_err(|source| RuntimeError::from_store("load child parent history", source))?;
            if !history
                .facts
                .runs
                .iter()
                .any(|run| run.run_id == request.parent_run_id)
            {
                self.get_run(GetRunRequest {
                    session_id: request.session_id.clone(),
                    run_id: request.parent_run_id.clone(),
                })
                .await?;
            }
        }
        let tasks = tasks
            .into_iter()
            .filter(|task| task.parent_run_id == request.parent_run_id);
        let mut snapshots = Vec::new();
        for task in tasks {
            snapshots.push(
                self.child_history_snapshot(&request.session_id, task)
                    .await?,
            );
        }
        Ok(ListChildTasksResult { tasks: snapshots })
    }

    pub async fn get_child_task(
        &self,
        request: GetChildTaskRequest,
    ) -> RuntimeResult<GetChildTaskResult> {
        self.session(&request.session_id).await?;
        let task = self
            .product_child_task(&request.session_id, &request.child_task_id)
            .await?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: request.session_id.clone(),
                child_task_id: request.child_task_id,
            })?;
        Ok(GetChildTaskResult {
            task: self
                .child_history_snapshot(&request.session_id, task)
                .await?,
        })
    }

    pub async fn cancel_child_task(
        &self,
        request: CancelChildTaskRequest,
    ) -> RuntimeResult<CancelChildTaskResult> {
        self.ensure_running()?;
        let session = self.session(&request.session_id).await?;
        let _parent = session.mutation().await;
        if let Some(history) = self
            .product_child_task(&request.session_id, &request.child_task_id)
            .await?
        {
            if history.inherited {
                return Err(RuntimeError::ChildTaskNotFound {
                    session_id: request.session_id,
                    child_task_id: request.child_task_id,
                });
            }
            if history.status.is_some_and(|status| status.is_terminal()) {
                return Ok(CancelChildTaskResult {
                    task: self
                        .child_history_snapshot(&request.session_id, history)
                        .await?,
                });
            }
        }
        let existing = self
            .child_tasks
            .get(&request.session_id, &request.child_task_id)?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: request.session_id.clone(),
                child_task_id: request.child_task_id.clone(),
            })?;
        if existing.status.is_terminal() {
            return Err(RuntimeError::SnapshotStale);
        }
        let stored = self
            .store
            .request_child_task_cancellation(&request.session_id, &request.child_task_id)
            .await
            .map_err(|source| {
                RuntimeError::from_store("request child task cancellation", source)
            })?;
        let projected = self.event_sender.publish_transition(|| {
            self.child_tasks.upsert(stored)?;
            let projected = self
                .child_tasks
                .get(&request.session_id, &request.child_task_id)?
                .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                    session_id: request.session_id.clone(),
                    child_task_id: request.child_task_id.clone(),
                })?;
            Ok((
                projected,
                Some(assistant_protocol::RuntimeEvent::SessionChanged {
                    session_id: request.session_id.clone(),
                }),
            ))
        })?;
        if !projected.status.is_terminal() {
            let _ = self.child_tasks.cancel_active(
                &request.session_id,
                &projected.parent_run_id,
                &request.child_task_id,
                crate::delegation::cancellation::ChildCancellationReason::Requested,
            )?;
        }
        Ok(CancelChildTaskResult {
            task: self.child_snapshot(projected).await?,
        })
    }

    pub(super) async fn child_snapshot(
        &self,
        task: StoredChildTask,
    ) -> RuntimeResult<ChildTaskSnapshot> {
        self.child_history_snapshot(&task.session_id, StoredChildTaskHistory::from_task(&task))
            .await
    }

    /// 文件历史直接投影产品值；不创建 StoredChildTask、不安装取消句柄或 Journal。
    pub(super) async fn child_history_snapshot(
        &self,
        session_id: &assistant_protocol::SessionId,
        task: StoredChildTaskHistory,
    ) -> RuntimeResult<ChildTaskSnapshot> {
        let status = task.status.ok_or(RuntimeError::InternalStateUnavailable {
            component: "child historical status",
        })?;
        let final_text = if task.body_unavailable {
            String::new()
        } else if let Some(message_id) = &task.final_message_id {
            let owner = assistant_protocol::ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: task.child_task_id.clone(),
            };
            let location = self
                .store
                .locate_conversation_message(crate::ConversationMessageLocationRequest {
                    owner: owner.clone(),
                    message_id: message_id.clone(),
                })
                .await
                .map_err(|source| RuntimeError::from_store("locate child final message", source))?
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "child final message",
                })?;
            if location.generation != task.body_generation {
                return Err(RuntimeError::SnapshotStale);
            }
            let start = usize::try_from(location.message_ordinal).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "child final message offset",
                }
            })?;
            let conversation = self
                .store
                .load_conversation_raw_window(crate::ConversationRawWindowRequest {
                    owner,
                    generation: task.body_generation,
                    start,
                    limit: 1,
                })
                .await
                .map_err(|source| RuntimeError::from_store("read child final message", source))?
                .conversation;
            conversation
                .messages
                .iter()
                .find_map(|message| match message {
                    ConversationMessage::Assistant(message) if &message.id == message_id => Some(
                        message
                            .parts
                            .iter()
                            .filter_map(|part| match part {
                                AssistantPart::Text(text) => Some(text.text.as_str()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok(ChildTaskSnapshot {
            inherited: task.inherited,
            body_unavailable: task.body_unavailable,
            child_task_id: task.child_task_id,
            session_id: session_id.clone(),
            parent_run_id: task.parent_run_id,
            parent_tool_call_id: task.parent_tool_call_id,
            title: task.title,
            status,
            variant: task.agent_variant,
            cancel_requested: task.cancel_requested,
            final_text,
            error: task.error,
            created_at_ms: task.created_at_ms,
            started_at_ms: task.started_at_ms,
            finished_at_ms: task.finished_at_ms,
        })
    }

    /// 本会话的文件目录与真实执行观察合并；只读 child 从不装配成任务。
    pub(super) async fn product_child_tasks(
        &self,
        session_id: &assistant_protocol::SessionId,
    ) -> RuntimeResult<Vec<StoredChildTaskHistory>> {
        let session = self.session(session_id).await?;
        let controls = self.child_tasks.list_for_session(session_id)?;
        let mut tasks = std::collections::BTreeMap::new();
        if session.lock_state()?.is_conversation_available {
            let history = self
                .store
                .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                    session_id: session_id.clone(),
                })
                .await
                .map_err(|source| {
                    RuntimeError::from_store("load child history directory", source)
                })?;
            if history.generation != session.lock_state()?.body_generation {
                return Err(RuntimeError::SnapshotStale);
            }
            for task in history.facts.child_tasks {
                if !task.inherited
                    && let Some(control) = controls
                        .iter()
                        .find(|control| control.child_task_id == task.child_task_id)
                {
                    if task.parent_run_id != control.parent_run_id
                        || task.parent_tool_call_id != control.parent_tool_call_id
                    {
                        return Err(RuntimeError::SnapshotStale);
                    }
                    if !control.status.is_terminal()
                        || !task.status.is_some_and(|status| status.is_terminal())
                    {
                        let task = self
                            .project_child_control(&session, control.clone(), true)
                            .await?;
                        tasks.insert(task.child_task_id.clone(), task);
                        continue;
                    }
                }
                // 父交换完成后目录已冻结 child 终态。列表复用该可靠快照，
                // 不为显示状态逐个打开历史 child；详情仍校验实际 child 文件。
                let task = if task.status.is_some_and(|status| status.is_terminal()) {
                    task
                } else {
                    self.read_child_history(session_id, task).await?.ok_or(
                        RuntimeError::InternalStateUnavailable {
                            component: "child file binding",
                        },
                    )?
                };
                tasks.insert(task.child_task_id.clone(), task);
            }
        }
        for task in controls {
            if tasks.contains_key(&task.child_task_id) {
                continue;
            }
            let task = self.project_child_control(&session, task, false).await?;
            tasks.insert(task.child_task_id.clone(), task);
        }
        let mut tasks: Vec<_> = tasks.into_values().collect();
        tasks.sort_by(|left, right| {
            (left.created_at_ms, &left.child_task_id)
                .cmp(&(right.created_at_ms, &right.child_task_id))
        });
        Ok(tasks)
    }

    pub(super) async fn product_child_task(
        &self,
        session_id: &assistant_protocol::SessionId,
        child_task_id: &assistant_protocol::ChildTaskId,
    ) -> RuntimeResult<Option<StoredChildTaskHistory>> {
        let session = self.session(session_id).await?;
        let control = self.child_tasks.get(session_id, child_task_id)?;
        if !session.lock_state()?.is_conversation_available
            && let Some(control) = control
        {
            return self
                .project_child_control(&session, control, false)
                .await
                .map(Some);
        }
        let history = self
            .store
            .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            })
            .await
            .map_err(|source| RuntimeError::from_store("load child history directory", source))?;
        if history.generation != session.lock_state()?.body_generation {
            return Err(RuntimeError::SnapshotStale);
        }
        let binding = history
            .facts
            .child_tasks
            .into_iter()
            .find(|task| &task.child_task_id == child_task_id);
        if let Some(control) = control
            && binding.as_ref().is_none_or(|binding| !binding.inherited)
        {
            if binding.as_ref().is_some_and(|binding| {
                binding.parent_run_id != control.parent_run_id
                    || binding.parent_tool_call_id != control.parent_tool_call_id
            }) {
                return Err(RuntimeError::SnapshotStale);
            }
            return self
                .project_child_control(&session, control, binding.is_some())
                .await
                .map(Some);
        }
        let Some(binding) = binding else {
            return Ok(None);
        };
        self.read_child_history(session_id, binding)
            .await?
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "child file binding",
            })
            .map(Some)
    }

    /// 实际 child 控制仅补充活动观察；已提交的文件终态优先于旧内存值。
    /// 本函数只短暂读状态锁，不取 mutation、不恢复执行；文件失败不能以缓存成功掩盖。
    /// 未开始的 accepted 文件可尚无描述；旧进程未 prepare 的中断及不可读 owner 保留诊断。
    async fn project_child_control(
        &self,
        session: &crate::session::SessionController,
        control: StoredChildTask,
        has_directory_binding: bool,
    ) -> RuntimeResult<StoredChildTaskHistory> {
        let observation = StoredChildTaskHistory::from_task(&control);
        if control.conversation_state == crate::StoredConversationState::Unavailable
            || !session.lock_state()?.is_conversation_available
        {
            return Ok(observation);
        }
        let reliable = self
            .read_child_history(&control.session_id, observation.clone())
            .await?;
        if let Some(reliable) = reliable {
            if reliable.status.is_some_and(|status| status.is_terminal()) {
                return Ok(reliable);
            }
            if !control.status.is_terminal()
                || !session.lock_state()?.execution_prepared
                    && control.status == assistant_protocol::ChildTaskStatus::Interrupted
            {
                return Ok(observation);
            }
            return Err(RuntimeError::SnapshotStale);
        }
        if control.status == assistant_protocol::ChildTaskStatus::Accepted && !has_directory_binding
        {
            return Ok(observation);
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "child file binding",
        })
    }

    async fn read_child_history(
        &self,
        session_id: &assistant_protocol::SessionId,
        binding: StoredChildTaskHistory,
    ) -> RuntimeResult<Option<StoredChildTaskHistory>> {
        if binding.inherited && binding.body_unavailable {
            return Ok(Some(binding));
        }
        let history = self
            .store
            .load_conversation_history(assistant_protocol::ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: binding.child_task_id.clone(),
            })
            .await
            .map_err(|source| RuntimeError::from_store("load child file history", source))?;
        let Some(mut task) = history
            .facts
            .child_tasks
            .into_iter()
            .find(|task| task.child_task_id == binding.child_task_id)
        else {
            return Ok(None);
        };
        if task.body_generation != history.generation
            || task.parent_run_id != binding.parent_run_id
            || task.parent_tool_call_id != binding.parent_tool_call_id
        {
            return Err(RuntimeError::SnapshotStale);
        }
        task.inherited |= binding.inherited;
        Ok(Some(task))
    }
}
