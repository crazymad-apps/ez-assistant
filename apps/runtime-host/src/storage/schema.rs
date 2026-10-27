//! 日常 Recall FTS 能力初始化；版本化结构变更只归 migrations。

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

const RECALL_FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS conversation_recall_fts USING fts5(
    normalized_text,
    content='conversation_recall_documents',
    content_rowid='document_rowid',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_ai AFTER INSERT ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(rowid, normalized_text)
    VALUES (new.document_rowid, new.normalized_text);
END;
CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_ad AFTER DELETE ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(conversation_recall_fts, rowid, normalized_text)
    VALUES ('delete', old.document_rowid, old.normalized_text);
END;
CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_au AFTER UPDATE ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(conversation_recall_fts, rowid, normalized_text)
    VALUES ('delete', old.document_rowid, old.normalized_text);
    INSERT INTO conversation_recall_fts(rowid, normalized_text)
    VALUES (new.document_rowid, new.normalized_text);
END;
"#;

/// 探测并初始化本机 SQLite 的 trigram FTS5 能力；失败只关闭 Recall，不阻断 Runtime。
pub(super) fn initialize_recall_fts(connection: &mut Connection) -> bool {
    let existed = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'conversation_recall_fts'",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some();
    let triggers_complete = connection.query_row(
        "SELECT COUNT(*)=3 FROM sqlite_schema WHERE type='trigger' AND name IN (
            'conversation_recall_documents_ai','conversation_recall_documents_ad','conversation_recall_documents_au')",
        [], |row| row.get::<_, bool>(0),
    ).unwrap_or(false);
    let Ok(transaction) = connection.transaction_with_behavior(TransactionBehavior::Immediate)
    else {
        return false;
    };
    // F02 的版本事务只替换普通派生表并移除触发器，不在受限迁移回调中启动 FTS。
    // 触发器缺失也意味着旧虚拟索引不再匹配正文表；在此事务一起重建，失败只关闭 Recall。
    if (!existed || !triggers_complete)
        && transaction
            .execute_batch(
                "DROP TRIGGER IF EXISTS conversation_recall_documents_ai;
                 DROP TRIGGER IF EXISTS conversation_recall_documents_ad;
                 DROP TRIGGER IF EXISTS conversation_recall_documents_au;
                 DROP TABLE IF EXISTS conversation_recall_fts;
                 DELETE FROM conversation_recall_documents;
                 UPDATE conversation_recall_heads
                 SET indexed_message_count = 0, state = 'dirty';",
            )
            .is_err()
    {
        return false;
    }
    if transaction.execute_batch(RECALL_FTS_SCHEMA).is_err() {
        return false;
    }
    // 重建任务属于上次进程；保留元信息，首次搜索再按既有有界流程重建，不扫描正文。
    if transaction
        .execute(
            "UPDATE conversation_recall_heads SET state = 'dirty' WHERE state = 'rebuilding'",
            [],
        )
        .is_err()
    {
        return false;
    }
    transaction.commit().is_ok()
}
