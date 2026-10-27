//! 父 pending 交换临时保存 child 文件位置；可靠终态及正文只从 child 文件读取。
//! 位置随父 receipt 删除，不作为产品历史目录或执行恢复句柄。

use agent_types::{
    AssistantPart, ConversationMessage, ToolMessage, ToolResult, ToolResultContent,
    ToolResultStatus,
};
use assistant_protocol::{
    ChildTaskId, ChildTaskStatus, ConversationOwner, RunId, SessionId, ToolCallId,
};
use assistant_runtime::StoredChildTaskHistory;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{PendingTable, child_status_code, delegation_error_result, tool_message_id};
use crate::storage::{
    StorageEngine, StorageResult, conflict, internal_error, invalid_data, invalid_data_with_source,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingChildLocation {
    child_task_id: ChildTaskId,
    body_generation: u64,
}

impl StorageEngine {
    /// 父交换尚未可靠闭合时，回执只提供临时阅读位置；事实仍逐一核对原 child 文件。
    pub(in crate::storage) fn pending_child_catalog(
        &mut self,
        session_id: &SessionId,
    ) -> StorageResult<Vec<StoredChildTaskHistory>> {
        let rows = self.connection.prepare(
            "SELECT e.run_id,s.call_id,s.delegated_child_json FROM pending_tool_starts s
             JOIN pending_tool_exchanges e ON e.receipt_id=s.receipt_id
             WHERE e.session_id=?1 AND s.delegated_child_json IS NOT NULL ORDER BY e.receipt_id,s.call_id"
        ).map_err(|source|internal_error("pending child catalogue could not be queried",source))?
            .query_map([session_id.as_str()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))
            .map_err(|source|internal_error("pending child catalogue could not be read",source))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source|internal_error("pending child catalogue row is invalid",source))?;
        rows.into_iter()
            .map(|(run, call, json)| {
                let run = RunId::new(run).map_err(|source| {
                    invalid_data_with_source("pending child Run ID is invalid", source)
                })?;
                let call = ToolCallId::new(call).map_err(|source| {
                    invalid_data_with_source("pending child call ID is invalid", source)
                })?;
                self.read_pending_child(session_id, &run, &call, &json)
            })
            .collect()
    }

    /// 结算前冻结现有父 receipt 身份。无父交换的 Store 级 child 不凭空创建回执；存在交换时
    /// 必须已有对应 delegate 的 started 事实，不能把另一工具的结果装进父恢复链路。
    pub(in crate::storage) fn pending_child_receipt(
        &self,
        session_id: &SessionId,
        child_task_id: &ChildTaskId,
    ) -> StorageResult<Option<String>> {
        let row = self
            .connection
            .query_row(
                "SELECT e.receipt_id,c.parent_tool_call_id FROM child_tasks c
             JOIN pending_tool_exchanges e ON e.session_id=c.session_id AND e.run_id=c.parent_run_id
             WHERE c.session_id=?1 AND c.child_task_id=?2",
                params![session_id.as_str(), child_task_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| {
                internal_error("child parent receipt could not be checked", source)
            })?;
        let Some((receipt, call)) = row else {
            return Ok(None);
        };
        let pending = self.load_pending_exchange(&receipt, PendingTable::Run)?;
        if !pending.started_calls.contains(&call) || !pending.assistant.parts.iter().any(|part| {
            matches!(part, AssistantPart::ToolCall(tool) if tool.id.as_str()==call && tool.name.as_str()==assistant_runtime::DELEGATE_TASK_TOOL_NAME)
        }) {
            return Err(invalid_data("child parent receipt has no started delegation"));
        }
        Ok(Some(receipt))
    }

    /// 父文件尚无可靠 child 目录时只用其 pending started 行定位；不查终态 child SQL。
    pub(in crate::storage) fn pending_child_history(
        &mut self,
        receipt: &str,
        session_id: &SessionId,
        run_id: &RunId,
    ) -> StorageResult<Vec<StoredChildTaskHistory>> {
        let rows = self
            .connection
            .prepare(
                "SELECT s.call_id,s.delegated_child_json FROM pending_tool_starts s
             JOIN pending_tool_exchanges e ON e.receipt_id=s.receipt_id
             WHERE e.receipt_id=?1 AND e.session_id=?2 AND e.run_id=?3
               AND s.delegated_child_json IS NOT NULL ORDER BY s.call_id",
            )
            .map_err(|source| {
                internal_error("pending child locations could not be queried", source)
            })?
            .query_map(
                params![receipt, session_id.as_str(), run_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|source| internal_error("pending child locations could not be read", source))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| internal_error("pending child location row is invalid", source))?;
        rows.into_iter()
            .map(|(call, json)| {
                let call = ToolCallId::new(call).map_err(|source| {
                    invalid_data_with_source("pending child call ID is invalid", source)
                })?;
                self.read_pending_child(session_id, run_id, &call, &json)
            })
            .collect()
    }

    /// 只生成现有父交换待提交的 ToolMessage，不启动 child 或重放工具。最终正文按文件位置
    /// 读取单条消息；调用方仍须完成原 staged append，成功前不能发布结果或释放父 receipt。
    pub(super) fn recovered_file_delegate_result(
        &mut self,
        receipt: &str,
        index: usize,
        session_id: &SessionId,
        run_id: &RunId,
        call: &agent_types::ToolCall,
    ) -> StorageResult<ToolMessage> {
        let json = self
            .connection
            .query_row(
                "SELECT s.delegated_child_json FROM pending_tool_starts s
             JOIN pending_tool_exchanges e ON e.receipt_id=s.receipt_id
             WHERE e.receipt_id=?1 AND e.session_id=?2 AND e.run_id=?3 AND s.call_id=?4",
                params![
                    receipt,
                    session_id.as_str(),
                    run_id.as_str(),
                    call.id.as_str()
                ],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|source| internal_error("pending child location could not be read", source))?
            .flatten();
        let id = tool_message_id(receipt, index)?;
        let Some(json) = json else {
            let has_child: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM child_tasks WHERE session_id=?1 AND parent_run_id=?2 AND parent_tool_call_id=?3)",
                params![session_id.as_str(),run_id.as_str(),call.id.as_str()], |row| row.get(0),
            ).map_err(|source| internal_error("unfinished delegated child could not be checked",source))?;
            if has_child {
                return Err(invalid_data(
                    "delegated child settlement is not yet reliable",
                ));
            }
            return Ok(delegation_error_result(
                id,
                call,
                None,
                "interrupted",
                "interrupted",
            ));
        };
        let task = self.read_pending_child(
            session_id,
            run_id,
            &ToolCallId::new(call.id.as_str()).map_err(|source| {
                invalid_data_with_source("delegated call ID is invalid", source)
            })?,
            &json,
        )?;
        let status = task
            .status
            .ok_or_else(|| invalid_data("pending child terminal status is missing"))?;
        if status != ChildTaskStatus::Completed {
            let status_code = child_status_code(status);
            let code = task
                .error
                .as_ref()
                .map(|error| crate::storage::run_projection::error_code_value(error.code))
                .unwrap_or(status_code);
            return Ok(delegation_error_result(
                id,
                call,
                Some(task.child_task_id.as_str()),
                status_code,
                code,
            ));
        }
        let final_message = task
            .final_message_id
            .ok_or_else(|| invalid_data("completed delegated child has no final message"))?;
        let path = self.child_body(session_id, &task.child_task_id, task.body_generation)?;
        let (ordinal, _) = self
            .conversation_indexes
            .locate_message(&path, &final_message)?
            .ok_or_else(|| invalid_data("delegated child final message is unavailable"))?;
        let (body, _, _) = self
            .conversation_indexes
            .read_raw_window(&path, ordinal, 1)?;
        let Some(ConversationMessage::Assistant(message)) = body.messages.first() else {
            return Err(invalid_data(
                "delegated child final message is not an assistant message",
            ));
        };
        let result = message
            .parts
            .iter()
            .filter_map(|part| match part {
                AssistantPart::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        Ok(ToolMessage {
            id,
            result: ToolResult {
                call_id: call.id.clone(),
                status: ToolResultStatus::Success,
                content: ToolResultContent::json(
                    serde_json::json!({"task_id":task.child_task_id.as_str(),"status":"completed","result":result}),
                ),
                metadata: None,
            },
        })
    }

    /// 暂存位置不是历史事实；每次读取仍核对文件 owner、代次、父 Run／call 及可靠终态。
    /// 有待提交 child 尾帧或任何绑定不符即失败，不查询终态 SQL 或补造任务作为兜底。
    fn read_pending_child(
        &mut self,
        session_id: &SessionId,
        run_id: &RunId,
        call_id: &ToolCallId,
        json: &str,
    ) -> StorageResult<StoredChildTaskHistory> {
        let location: PendingChildLocation = serde_json::from_str(json).map_err(|source| {
            invalid_data_with_source("pending child location is invalid", source)
        })?;
        crate::storage::filesystem::validate_child_task_component(&location.child_task_id)?;
        let owner = ConversationOwner::ChildTask {
            session_id: session_id.clone(),
            child_task_id: location.child_task_id.clone(),
        };
        self.ensure_no_pending_history(&owner)?;
        let path = self.child_body(
            session_id,
            &location.child_task_id,
            location.body_generation,
        )?;
        let task = self
            .conversation_indexes
            .read_child_history(
                &path,
                &owner,
                location.body_generation,
                Some(&location.child_task_id),
            )?
            .into_iter()
            .find(|task| task.child_task_id == location.child_task_id)
            .ok_or_else(|| invalid_data("pending child file has no owner history"))?;
        if &task.parent_run_id != run_id
            || &task.parent_tool_call_id != call_id
            || task.inherited
            || task.body_generation != location.body_generation
            || !task.status.is_some_and(|status| status.is_terminal())
        {
            return Err(invalid_data(
                "pending child file binding or terminal state does not match",
            ));
        }
        Ok(task)
    }
}

/// child 文件已经同步；在同一结算事务内把阅读位置交给既有父 started 行。
/// 前置回滚预检使用同一算法，失败不发布位置；父 receipt 删除后由原外键清理该临时值。
pub(in crate::storage) fn freeze_child_for_pending(
    transaction: &Transaction<'_>,
    receipt: &str,
    session_id: &SessionId,
    child_task_id: &ChildTaskId,
) -> StorageResult<()> {
    let (call, generation): (String, i64) = transaction
        .query_row(
            "SELECT c.parent_tool_call_id,c.body_generation FROM child_tasks c
         JOIN pending_tool_exchanges e ON e.session_id=c.session_id AND e.run_id=c.parent_run_id
         WHERE c.session_id=?1 AND c.child_task_id=?2 AND e.receipt_id=?3",
            params![session_id.as_str(), child_task_id.as_str(), receipt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|source| {
            internal_error("child parent receipt changed before settlement", source)
        })?;
    let json = serde_json::to_string(&PendingChildLocation {
        child_task_id: child_task_id.clone(),
        body_generation: crate::storage::positive_u64(
            generation,
            "pending child generation is invalid",
        )?,
    })
    .map_err(|source| internal_error("pending child location could not be encoded", source))?;
    let changed = transaction.execute(
        "UPDATE pending_tool_starts SET delegated_child_json=?1 WHERE receipt_id=?2 AND call_id=?3
         AND (delegated_child_json IS NULL OR delegated_child_json=?1)",params![json,receipt,call],
    ).map_err(|source| internal_error("pending child location could not be committed",source))?;
    if changed != 1 {
        return Err(conflict(
            "child parent started receipt changed before settlement",
        ));
    }
    Ok(())
}

/// child 帧已同步且自身回执已显式删除后，既有父 pending 位置接替控制；父交换闭合后
/// 则由父文件目录承接。只释放核验过的终态，不级联丢失待提交证据或解码过期执行载荷。
/// 整个完成事务失败时原回执和控制一并保留，重开仍完成同一 staged append。
pub(in crate::storage) fn release_parent_child_controls(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
    run_id: &RunId,
    children: &[StoredChildTaskHistory],
) -> StorageResult<()> {
    for child in children {
        if child.inherited
            || &child.parent_run_id != run_id
            || !child.status.is_some_and(|status| status.is_terminal())
        {
            return Err(invalid_data(
                "parent child history is not a reliable terminal",
            ));
        }
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM child_tasks WHERE session_id=?1 AND child_task_id=?2)",
                params![session_id.as_str(), child.child_task_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|source| {
                internal_error("parent child control could not be checked", source)
            })?;
        // 已释放的控制无需重建；文件事实与父回执仍足以完成恢复。
        if !exists {
            continue;
        }
        let changed = transaction
            .execute(
                "DELETE FROM child_tasks WHERE session_id=?1 AND child_task_id=?2
             AND parent_run_id=?3 AND parent_tool_call_id=?4 AND body_generation=?5
             AND status IN ('completed','failed','cancelled','interrupted')
             AND NOT EXISTS(SELECT 1 FROM child_body_appends WHERE child_task_id=?2)
             AND NOT EXISTS(SELECT 1 FROM child_pending_tool_exchanges WHERE child_task_id=?2)",
                params![
                    session_id.as_str(),
                    child.child_task_id.as_str(),
                    run_id.as_str(),
                    child.parent_tool_call_id.as_str(),
                    crate::storage::to_i64(
                        child.body_generation,
                        "child generation exceeds SQLite range"
                    )?
                ],
            )
            .map_err(|source| {
                internal_error("parent child control could not be released", source)
            })?;
        if changed != 1 {
            return Err(conflict(
                "parent child control changed or still has unfinished work",
            ));
        }
    }
    Ok(())
}
