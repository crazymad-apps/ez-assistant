//! 无父工具回执的 child：两个既有 staged append 同步后，共用一个 SQL 完成事务。

use assistant_runtime::{StoredChildTaskHistory, StoredHistoryFacts};

use super::*;

impl StorageEngine {
    pub(super) fn complete_standalone_child(&mut self, staged: StagedAppend) -> StorageResult<()> {
        let child_operation_id = staged.operation_id.clone();
        drop(staged);
        let prepared = self.prepare_target_append(&child_operation_id, AppendTable::ChildTask)?;
        let fact = standalone_child_fact(&prepared)?.clone();
        let ConversationStorageTarget::ChildTask { session_id, .. } = &prepared.staged.target
        else {
            return Err(invalid_data("child directory target is invalid"));
        };
        let session_id = session_id.clone();
        let created_at_ms = prepared.staged.created_at_ms;
        // 只保留目录事实，不在两个 owner 完成期间同时持有两份 child 正文/载荷。
        drop(prepared);
        let operation_id = format!("child-directory:{child_operation_id}");
        let exists: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM body_appends WHERE operation_id=?1)",
                [&operation_id],
                |row| row.get(0),
            )
            .map_err(|source| {
                internal_error("child directory receipt could not be checked", source)
            })?;
        if exists {
            let directory = self.staged_append(&operation_id, AppendTable::Session)?;
            if !matches!((&directory.target,&directory.purpose),
                (ConversationStorageTarget::SessionMetadata {session_id: owner},AppendPurpose::ChildDirectory {child_operation_id: dependency,child})
                if owner==&session_id && dependency==&child_operation_id && **child==fact)
            {
                return Err(invalid_data(
                    "child directory receipt identity is already in use",
                ));
            }
        } else {
            self.stage_target_append(
                operation_id.clone(),
                ConversationStorageTarget::SessionMetadata {
                    session_id: session_id.clone(),
                },
                Vec::new(),
                None,
                created_at_ms,
                AppendPurpose::ChildDirectory {
                    child_operation_id,
                    child: Box::new(fact),
                },
            )?;
        }
        self.complete_target_append(&operation_id, AppendTable::Session)
    }

    /// 任一回执都可在重启时进入同一完成入口；文件写完但 SQL 失败时不公开半份目录或终态。
    pub(super) fn complete_child_directory(&mut self, staged: StagedAppend) -> StorageResult<()> {
        let AppendPurpose::ChildDirectory {
            child_operation_id,
            child,
        } = &staged.purpose
        else {
            return Err(invalid_data("child directory purpose is invalid"));
        };
        let directory = self.prepare_target_append(&staged.operation_id, AppendTable::Session)?;
        let terminal = self.prepare_target_append(child_operation_id, AppendTable::ChildTask)?;
        let fact = standalone_child_fact(&terminal)?;
        let ConversationStorageTarget::SessionMetadata { session_id } = &directory.staged.target
        else {
            return Err(invalid_data("child directory has a Run target"));
        };
        let ConversationStorageTarget::ChildTask {
            session_id: child_session,
            ..
        } = &terminal.staged.target
        else {
            return Err(invalid_data("child directory dependency is not a child"));
        };
        let expected = StoredHistoryFacts {
            child_tasks: vec![fact.clone()],
            ..Default::default()
        };
        if session_id != child_session
            || **child != *fact
            || directory.batch.history.as_ref() != Some(&expected)
            || !directory.batch.conversation.messages.is_empty()
            || directory.staged.message_count_delta != 0
            || directory.staged.created_at_ms != terminal.staged.created_at_ms
        {
            return Err(invalid_data(
                "child directory and terminal receipts do not match",
            ));
        }
        self.write_target_append(child_operation_id, AppendTable::ChildTask)?;
        self.write_target_append(&staged.operation_id, AppendTable::Session)?;
        self.finalize_prepared_appends(vec![terminal, directory])
    }
}

fn standalone_child_fact(append: &PreparedAppend) -> StorageResult<&StoredChildTaskHistory> {
    let ConversationStorageTarget::ChildTask { child_task_id, .. } = &append.staged.target else {
        return Err(invalid_data("standalone child target is invalid"));
    };
    let AppendPurpose::ChildSettlement {
        status,
        error,
        final_message_id,
        parent_exchange_receipt: None,
        ..
    } = &append.staged.purpose
    else {
        return Err(invalid_data(
            "child directory dependency is not a standalone settlement",
        ));
    };
    let children = &append
        .batch
        .history
        .as_ref()
        .ok_or_else(|| invalid_data("standalone child history is missing"))?
        .child_tasks;
    let [child] = children.as_slice() else {
        return Err(invalid_data("standalone child has ambiguous owner history"));
    };
    if child.child_task_id != *child_task_id
        || child.inherited
        || child.body_generation != append.staged.body_generation
        || child.status != Some(*status)
        || !status.is_terminal()
        || child.error != *error
        || child.final_message_id != *final_message_id
        || child.finished_at_ms != Some(append.staged.created_at_ms)
    {
        return Err(invalid_data(
            "standalone child terminal history does not match its receipt",
        ));
    }
    Ok(child)
}
