//! 子任务公共查询、取消与规范正文派生投影。

use agent_types::{AssistantPart, ConversationMessage};
use assistant_protocol::{
    CancelChildTaskRequest, CancelChildTaskResult, ChildTaskSnapshot, GetChildTaskRequest,
    GetChildTaskResult, GetRunRequest, ListChildTasksRequest, ListChildTasksResult,
};

use super::AssistantRuntime;
use crate::{RuntimeError, RuntimeResult, StoredChildTask};

impl AssistantRuntime {
    pub(super) async fn child_task_conversation_snapshot(
        &self,
        session_id: &assistant_protocol::SessionId,
        child_task_id: &assistant_protocol::ChildTaskId,
    ) -> RuntimeResult<agent_types::ConversationSnapshot> {
        self.session(session_id)?;
        self.child_tasks
            .get(session_id, child_task_id)?
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
        self.get_run(GetRunRequest {
            session_id: request.session_id.clone(),
            run_id: request.parent_run_id.clone(),
        })
        .await?;
        let tasks = self
            .child_tasks
            .list(&request.session_id, &request.parent_run_id)?;
        let mut snapshots = Vec::with_capacity(tasks.len());
        for task in tasks {
            snapshots.push(self.child_snapshot(task).await?);
        }
        Ok(ListChildTasksResult { tasks: snapshots })
    }

    pub async fn get_child_task(
        &self,
        request: GetChildTaskRequest,
    ) -> RuntimeResult<GetChildTaskResult> {
        self.session(&request.session_id)?;
        let task = self
            .child_tasks
            .get(&request.session_id, &request.child_task_id)?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: request.session_id,
                child_task_id: request.child_task_id,
            })?;
        Ok(GetChildTaskResult {
            task: self.child_snapshot(task).await?,
        })
    }

    pub async fn cancel_child_task(
        &self,
        request: CancelChildTaskRequest,
    ) -> RuntimeResult<CancelChildTaskResult> {
        self.ensure_running()?;
        self.session(&request.session_id)?;
        let existing = self
            .child_tasks
            .get(&request.session_id, &request.child_task_id)?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: request.session_id.clone(),
                child_task_id: request.child_task_id.clone(),
            })?;
        if existing.status.is_terminal() {
            return Ok(CancelChildTaskResult {
                task: self.child_snapshot(existing).await?,
            });
        }
        let stored = self
            .store
            .request_child_task_cancellation(&request.session_id, &request.child_task_id)
            .await
            .map_err(|source| {
                RuntimeError::from_store("request child task cancellation", source)
            })?;
        self.child_tasks.upsert(stored.clone())?;
        let projected = self
            .child_tasks
            .get(&request.session_id, &request.child_task_id)?
            .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                session_id: request.session_id.clone(),
                child_task_id: request.child_task_id.clone(),
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
        let final_text = if task.final_message_id.is_some() {
            let conversation = self
                .store
                .load_child_conversation(&task.session_id, &task.child_task_id)
                .await
                .map_err(|source| {
                    RuntimeError::from_store("load child task conversation", source)
                })?;
            conversation
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    ConversationMessage::Assistant(message)
                        if task.final_message_id.as_ref() == Some(&message.id) =>
                    {
                        Some(
                            message
                                .parts
                                .iter()
                                .filter_map(|part| match part {
                                    AssistantPart::Text(text) => Some(text.text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        )
                    }
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok(ChildTaskSnapshot {
            child_task_id: task.child_task_id,
            session_id: task.session_id,
            parent_run_id: task.parent_run_id,
            parent_tool_call_id: task.parent_tool_call_id,
            title: task.title,
            status: task.status,
            variant: task.agent_variant,
            cancel_requested: task.cancel_requested,
            final_text,
            error: task.error,
            created_at_ms: task.created_at_ms,
            started_at_ms: task.started_at_ms,
            finished_at_ms: task.finished_at_ms,
        })
    }
}
