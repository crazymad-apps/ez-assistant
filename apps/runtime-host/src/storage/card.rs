//! 卡片按可靠工具收据去重；私有 UI 数据不进入 Conversation 文件或全文索引。

use assistant_protocol::{
    ConversationOwner, ListMessageCardsRequest, ListMessageCardsResult, MessageCardSnapshot,
    MessageCardSummary, SaveMessageCardSnapshotResult,
};
use assistant_runtime::{
    MessageCardSnapshotWrite, NewMessageCard, StoredMessageCard, message_card_owner,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
};

#[derive(Deserialize, Serialize)]
struct Payload {
    arguments: Value,
    tool_result: Value,
    source_fingerprint: Option<String>,
}

const SUMMARY_COLUMNS: &str =
    "position,card_id,run_id,anchor_message_id,title,source_json,snapshot_revision,inherited";

/// 目标 Session 尚未提交；只复制保留正文/子历史的卡片与最新快照，不继承页面实例或执行身份。
pub(super) fn copy_for_fork(
    tx: &rusqlite::Transaction<'_>,
    source: &assistant_protocol::SessionId,
    target: &assistant_protocol::SessionId,
    messages: &std::collections::BTreeSet<String>,
    children: &[String],
) -> StorageResult<()> {
    let messages = serde_json::to_string(messages)
        .map_err(|e| internal_error("fork card anchors could not be encoded", e))?;
    let children = serde_json::to_string(children)
        .map_err(|e| internal_error("fork card children could not be encoded", e))?;
    let ids=tx.prepare("SELECT card_id FROM message_cards WHERE session_id=?1 AND ((child_key='' AND anchor_message_id IN(SELECT value FROM json_each(?2))) OR child_key IN(SELECT value FROM json_each(?3))) ORDER BY position")
        .map_err(|e|internal_error("fork card query could not be prepared",e))?.query_map(params![source.as_str(),messages,children],|row|row.get::<_,String>(0))
        .map_err(|e|internal_error("fork cards could not be read",e))?.collect::<Result<Vec<_>,_>>().map_err(|e|internal_error("fork card identity invalid",e))?;
    for id in ids {
        let next = format!(
            "card-fork-{:x}",
            Sha256::digest(format!("{}\0{id}", target.as_str()))
        );
        tx.execute("INSERT INTO message_cards(card_id,session_id,child_key,run_id,anchor_message_id,receipt_id,tool_call_id,title,source_json,payload_json,snapshot_json,snapshot_schema,snapshot_revision,inherited)
            SELECT ?1,?2,child_key,run_id,anchor_message_id,receipt_id,tool_call_id,title,source_json,payload_json,snapshot_json,snapshot_schema,snapshot_revision,1 FROM message_cards WHERE card_id=?3 AND session_id=?4",
            params![next,target.as_str(),id,source.as_str()]).map_err(|e|database_write_error("fork card could not be copied",e))?;
    }
    Ok(())
}

impl StorageEngine {
    /// 当前 started 调用已由 Core 可靠提交；在同一事务中核对收据、去重并插入卡片。
    /// 不修改 Tool Result 或重放远端副作用；提交后由 Runtime 发布观察事件。
    pub(super) fn publish_message_card(
        &mut self,
        card: NewMessageCard,
    ) -> StorageResult<StoredMessageCard> {
        let (session, child) = message_card_owner(&card.owner);
        if card.card_id.is_empty()
            || card.card_id.len() > 128
            || card.title.trim().is_empty()
            || card.title.len() > 512
            || child.is_none() != card.run_id.is_some()
        {
            return Err(conflict("message card identity is invalid"));
        }
        let payload = serde_json::to_string(&Payload {
            arguments: card.arguments,
            tool_result: card.tool_result,
            source_fingerprint: card.source_fingerprint,
        })
        .map_err(|e| internal_error("card payload could not be encoded", e))?;
        if payload.len() > 1024 * 1024 {
            return Err(conflict("message card payload exceeds limit"));
        }
        let source = serde_json::to_string(&card.source)
            .map_err(|e| internal_error("card source could not be encoded", e))?;
        if source.len() > 8192 {
            return Err(conflict("message card source exceeds limit"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| database_write_error("card transaction could not begin", e))?;
        let pending = if let Some(child) = child {
            transaction.query_row(
                "SELECT e.receipt_id,e.assistant_json FROM child_pending_tool_exchanges e
                JOIN child_pending_tool_starts s USING(receipt_id)
                WHERE e.session_id=?1 AND e.child_task_id=?2 AND s.call_id=?3 AND e.state='begun'",
                params![session.as_str(), child.as_str(), card.tool_call_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
        } else {
            transaction.query_row(
                "SELECT e.receipt_id,e.assistant_json FROM pending_tool_exchanges e
                JOIN pending_tool_starts s USING(receipt_id)
                WHERE e.session_id=?1 AND e.run_id=?2 AND s.call_id=?3 AND e.state='begun'",
                params![
                    session.as_str(),
                    card.run_id.as_ref().map(|id| id.as_str()),
                    card.tool_call_id.as_str()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
        }
        .optional()
        .map_err(|e| internal_error("card tool identity could not be checked", e))?
        .ok_or_else(|| conflict("message card requires a started tool call"))?;
        let existing = transaction.query_row("SELECT card_id FROM message_cards WHERE session_id=?1 AND child_key=?2 AND receipt_id=?3 AND tool_call_id=?4",
            params![session.as_str(),child.map(|id|id.as_str()).unwrap_or(""),pending.0,card.tool_call_id.as_str()],|row|row.get::<_,String>(0))
            .optional().map_err(|e|internal_error("card receipt could not be checked",e))?;
        let result = if let Some(id) = existing {
            read_card(&transaction, &card.owner, &id)?
                .ok_or_else(|| invalid_data("card receipt is missing"))?
        } else {
            let assistant: agent_types::AssistantMessage = serde_json::from_str(&pending.1)
                .map_err(|e| internal_error("card anchor could not be read", e))?;
            transaction.execute("INSERT INTO message_cards(card_id,session_id,child_key,run_id,anchor_message_id,receipt_id,tool_call_id,title,source_json,payload_json)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![card.card_id,session.as_str(),child.map(|id|id.as_str()).unwrap_or(""),
                    card.run_id.as_ref().map(|id|id.as_str()),assistant.id.as_str(),pending.0,card.tool_call_id.as_str(),card.title,source,payload])
                .map_err(|e|database_write_error("message card could not be published",e))?;
            read_card(&transaction, &card.owner, &card.card_id)?
                .ok_or_else(|| invalid_data("published card is missing"))?
        };
        transaction
            .commit()
            .map_err(|e| database_write_error("card transaction could not commit", e))?;
        Ok(result)
    }

    pub(super) fn list_message_cards(
        &self,
        request: ListMessageCardsRequest,
    ) -> StorageResult<ListMessageCardsResult> {
        if !(1..=100).contains(&request.limit)
            || request.message_ids.len() > 1000
            || request.run_ids.len() > 1000
        {
            return Err(conflict("message card query exceeds limit"));
        }
        let (session, child) = message_card_owner(&request.owner);
        let messages = serde_json::to_string(&request.message_ids)
            .map_err(|e| internal_error("card query could not be encoded", e))?;
        let runs = serde_json::to_string(&request.run_ids)
            .map_err(|e| internal_error("card query could not be encoded", e))?;
        let after = i64::try_from(request.after.unwrap_or(0))
            .map_err(|_| conflict("card cursor exceeds limit"))?;
        let sql=format!("SELECT {SUMMARY_COLUMNS} FROM message_cards WHERE session_id=?1 AND child_key=?2 AND position>?3
            AND (?2<>'' OR anchor_message_id IN(SELECT value FROM json_each(?4)) OR run_id IN(SELECT value FROM json_each(?5))) ORDER BY position LIMIT ?6");
        let mut query = self
            .connection
            .prepare(&sql)
            .map_err(|e| internal_error("card query could not be prepared", e))?;
        let rows = query
            .query_map(
                params![
                    session.as_str(),
                    child.map(|id| id.as_str()).unwrap_or(""),
                    after,
                    messages,
                    runs,
                    request.limit + 1
                ],
                read_summary_row,
            )
            .map_err(|e| internal_error("message cards could not be queried", e))?;
        let mut cards = Vec::new();
        for row in rows {
            cards.push(summary(
                row.map_err(|e| internal_error("card summary could not be read", e))?,
                request.owner.clone(),
            )?);
        }
        let next = if cards.len() > request.limit as usize {
            cards.pop();
            cards.last().map(|card| card.position)
        } else {
            None
        };
        Ok(ListMessageCardsResult { cards, next })
    }

    pub(super) fn get_message_card(
        &self,
        owner: ConversationOwner,
        id: String,
    ) -> StorageResult<Option<StoredMessageCard>> {
        read_card(&self.connection, &owner, &id)
    }

    /// Runtime 已在 Session gate 与页面实例边界核实来源。事务只执行快照 CAS；
    /// 相同值重试 no-op，冲突返回当前可靠值，不追加正文或改变模型上下文。
    pub(super) fn save_message_card_snapshot(
        &mut self,
        write: MessageCardSnapshotWrite,
    ) -> StorageResult<SaveMessageCardSnapshotResult> {
        let json = serde_json::to_string(&write.snapshot.value)
            .map_err(|e| internal_error("snapshot could not be encoded", e))?;
        if json.len() > 256 * 1024
            || write.snapshot.schema.is_empty()
            || write.snapshot.schema.len() > 64
        {
            return Err(conflict("snapshot exceeds limit"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| database_write_error("snapshot transaction could not begin", e))?;
        let current = read_card(&transaction, &write.owner, &write.card_id)?
            .ok_or_else(|| conflict("message card does not exist"))?;
        if current.summary.snapshot_revision != write.expected_revision {
            return Ok(SaveMessageCardSnapshotResult {
                saved: false,
                revision: current.summary.snapshot_revision,
                snapshot: current.snapshot,
            });
        }
        if current.snapshot.as_ref() == Some(&write.snapshot) {
            return Ok(SaveMessageCardSnapshotResult {
                saved: true,
                revision: current.summary.snapshot_revision,
                snapshot: current.snapshot,
            });
        }
        let revision = current
            .summary
            .snapshot_revision
            .checked_add(1)
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(|| conflict("snapshot revision exceeds limit"))?;
        transaction.execute("UPDATE message_cards SET snapshot_json=?1,snapshot_schema=?2,snapshot_revision=?3 WHERE card_id=?4",
            params![json,write.snapshot.schema,revision,write.card_id]).map_err(|e|database_write_error("snapshot could not be saved",e))?;
        transaction
            .commit()
            .map_err(|e| database_write_error("snapshot transaction could not commit", e))?;
        Ok(SaveMessageCardSnapshotResult {
            saved: true,
            revision: revision as u64,
            snapshot: Some(write.snapshot),
        })
    }
}

type SummaryRow = (
    i64,
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    bool,
);

fn read_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn summary(row: SummaryRow, owner: ConversationOwner) -> StorageResult<MessageCardSummary> {
    Ok(MessageCardSummary {
        position: u64::try_from(row.0).map_err(|_| invalid_data("card position invalid"))?,
        card_id: row.1,
        owner,
        run_id: row
            .2
            .map(assistant_protocol::RunId::new)
            .transpose()
            .map_err(|_| invalid_data("card run invalid"))?,
        anchor_message_id: assistant_protocol::MessageId::new(row.3)
            .map_err(|_| invalid_data("card anchor invalid"))?,
        title: row.4,
        source: serde_json::from_str(&row.5)
            .map_err(|e| internal_error("card source invalid", e))?,
        snapshot_revision: u64::try_from(row.6)
            .map_err(|_| invalid_data("card revision invalid"))?,
        inherited: row.7,
    })
}

fn read_card(
    connection: &Connection,
    owner: &ConversationOwner,
    id: &str,
) -> StorageResult<Option<StoredMessageCard>> {
    let (session, child) = message_card_owner(owner);
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS},receipt_id,tool_call_id,payload_json,snapshot_schema,snapshot_json FROM message_cards WHERE session_id=?1 AND child_key=?2 AND card_id=?3"
    );
    let row = connection
        .query_row(
            &sql,
            params![
                session.as_str(),
                child.map(|id| id.as_str()).unwrap_or(""),
                id
            ],
            |row| {
                Ok((
                    read_summary_row(row)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                ))
            },
        )
        .optional()
        .map_err(|e| internal_error("card could not be read", e))?;
    row.map(|(record, receipt, call, payload, schema, json)| {
        let payload: Payload = serde_json::from_str(&payload)
            .map_err(|e| internal_error("card payload invalid", e))?;
        let snapshot = match (schema, json) {
            (Some(schema), Some(json)) => Some(MessageCardSnapshot {
                schema,
                value: serde_json::from_str(&json)
                    .map_err(|e| internal_error("snapshot invalid", e))?,
            }),
            (None, None) => None,
            _ => return Err(invalid_data("snapshot binding invalid")),
        };
        Ok(StoredMessageCard {
            summary: summary(record, owner.clone())?,
            receipt_id: receipt,
            tool_call_id: assistant_protocol::ToolCallId::new(call)
                .map_err(|_| invalid_data("card call invalid"))?,
            arguments: payload.arguments,
            tool_result: payload.tool_result,
            source_fingerprint: payload.source_fingerprint,
            snapshot,
        })
    })
    .transpose()
}
