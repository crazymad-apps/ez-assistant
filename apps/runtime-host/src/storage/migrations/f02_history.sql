-- 同一 F02 切换事务内收敛跨介质回执；旧回执必须在文件准备前可靠闭合。
-- 只保留原接纳身份；清空／重新输入不会将后来的输入误认为原物化结果。
-- 不建 Input 外键，原历史被用户移除后仍须保留 key 的过期身份。
ALTER TABLE sessions ADD COLUMN materialization_input_id TEXT;
-- 只暂存父交换尚未闭合时的 child 文件位置，随原 receipt 级联释放。
ALTER TABLE pending_tool_starts ADD COLUMN delegated_child_json TEXT;
CREATE TABLE body_appends_f02 (
    operation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id TEXT REFERENCES runs(run_id) ON DELETE CASCADE,
    body_generation INTEGER NOT NULL,
    base_byte_length INTEGER NOT NULL CHECK(base_byte_length >= 0),
    kind TEXT NOT NULL,
    payload BLOB NOT NULL,
    message_count_delta INTEGER NOT NULL CHECK(message_count_delta >= 0),
    message_step INTEGER CHECK(message_step IS NULL OR message_step > 0),
    created_at_ms INTEGER NOT NULL
);
INSERT INTO body_appends_f02 SELECT * FROM body_appends;
DROP TABLE body_appends;
ALTER TABLE body_appends_f02 RENAME TO body_appends;
CREATE TABLE child_body_appends_f02 (
    operation_id TEXT PRIMARY KEY,
    child_task_id TEXT NOT NULL UNIQUE REFERENCES child_tasks(child_task_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    body_generation INTEGER NOT NULL,
    base_byte_length INTEGER NOT NULL CHECK(base_byte_length >= 0),
    kind TEXT NOT NULL,
    payload BLOB NOT NULL,
    message_count_delta INTEGER NOT NULL CHECK(message_count_delta >= 0),
    message_step INTEGER CHECK(message_step IS NULL OR message_step > 0),
    created_at_ms INTEGER NOT NULL
);
INSERT INTO child_body_appends_f02 SELECT * FROM child_body_appends;
DROP TABLE child_body_appends;
ALTER TABLE child_body_appends_f02 RENAME TO child_body_appends;

-- 全部 owner 的文件事实准备及校验完成后，和指针切换同一事务释放旧历史关联。
DROP TABLE run_message_refs;
DROP TABLE message_feedback;

-- 独立记账保留真实请求去重与全部用量；Run 归属只存在会话文件。
ALTER TABLE model_request_records DROP COLUMN run_id;

-- 历史选择已经进入所属 owner 文件；只保留真实待消费输入的临时选择。
DELETE FROM skill_activations
WHERE NOT EXISTS(SELECT 1 FROM inputs i WHERE i.input_id=skill_activations.input_id
                 AND i.session_id=skill_activations.session_id AND i.state='queued');
DELETE FROM mcp_input_selections
WHERE NOT EXISTS(SELECT 1 FROM inputs i WHERE i.input_id=mcp_input_selections.input_id
                 AND i.session_id=mcp_input_selections.session_id AND i.state='queued');
UPDATE inputs SET skill_activation_json=NULL WHERE state='committed';

-- 终态问题已随全部 owner 迁入文件；待回答控制独立于已结束的父 Run 生命周期。
CREATE TABLE session_feedback_f02 (
    request_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json) AND json_type(snapshot_json)='object'
        AND json_extract(snapshot_json,'$.status') IS 'pending')
);
INSERT INTO session_feedback_f02 SELECT * FROM session_feedback
    WHERE json_extract(snapshot_json,'$.status')='pending';
DROP TABLE session_feedback;
ALTER TABLE session_feedback_f02 RENAME TO session_feedback;
CREATE INDEX session_feedback_session ON session_feedback(session_id);
CREATE UNIQUE INDEX session_feedback_pending ON session_feedback(session_id)
    WHERE json_extract(snapshot_json,'$.status')='pending';

-- Recall 只保留文本检索定位；child 的存在与代次由文件目录证明，不能依赖终态控制外键。
-- 先移除 FTS 触发器，再替换派生表，避免版本事务调用 FTS 模块内部初始化。
-- 旧虚拟索引留到正常 Recall 初始化发现触发器缺失后整体重建；它不是读取授权或历史来源。
DROP TRIGGER IF EXISTS conversation_recall_documents_ai;
DROP TRIGGER IF EXISTS conversation_recall_documents_ad;
DROP TRIGGER IF EXISTS conversation_recall_documents_au;
DROP TABLE conversation_recall_documents;
DROP TABLE conversation_recall_heads;
CREATE TABLE conversation_recall_documents (
    document_rowid INTEGER PRIMARY KEY,
    document_id TEXT NOT NULL UNIQUE,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('session','child_task')),
    owner_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    child_task_id TEXT,
    body_generation INTEGER NOT NULL CHECK(body_generation > 0),
    message_id TEXT NOT NULL,
    message_kind TEXT NOT NULL CHECK(message_kind IN ('user','assistant')),
    message_ordinal INTEGER NOT NULL CHECK(message_ordinal >= 0),
    created_at_ms INTEGER NOT NULL,
    normalized_text TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    UNIQUE(session_id,owner_kind,owner_id,body_generation,message_id)
);
CREATE INDEX conversation_recall_documents_owner
    ON conversation_recall_documents(session_id,owner_kind,owner_id,body_generation,message_ordinal);
CREATE INDEX conversation_recall_documents_session
    ON conversation_recall_documents(session_id,child_task_id,body_generation);
CREATE TABLE conversation_recall_heads (
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('session','child_task')),
    owner_id TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    child_task_id TEXT,
    body_generation INTEGER NOT NULL CHECK(body_generation > 0),
    indexed_message_count INTEGER NOT NULL CHECK(indexed_message_count >= 0),
    state TEXT NOT NULL CHECK(state IN ('ready','dirty','rebuilding','unavailable')),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY(session_id,owner_kind,owner_id)
);

-- 全部 parent／child 文件已准备且指针已切换；派生索引已移除 child 外键及 FTS 触发器。
-- 只释放已迁出的可靠终态，未知状态由最终校验拒绝，不扩大到活动控制或排队输入。
DELETE FROM child_tasks
WHERE status IN ('completed','failed','cancelled','interrupted')
  AND NOT EXISTS(SELECT 1 FROM child_body_appends a WHERE a.child_task_id=child_tasks.child_task_id)
  AND NOT EXISTS(SELECT 1 FROM child_pending_tool_exchanges e WHERE e.child_task_id=child_tasks.child_task_id);

-- 旧父终态同样只在全部文件迁出且 child 控制已收敛后释放，不靠级联清掉恢复证据。
DELETE FROM runs
WHERE status IN ('completed','failed','cancelled','interrupted')
  AND NOT EXISTS(SELECT 1 FROM body_appends a WHERE a.run_id=runs.run_id)
  AND NOT EXISTS(SELECT 1 FROM pending_tool_exchanges e WHERE e.run_id=runs.run_id)
  AND NOT EXISTS(SELECT 1 FROM child_tasks c WHERE c.parent_run_id=runs.run_id)
  AND NOT EXISTS(SELECT 1 FROM skill_activations a WHERE a.run_id=runs.run_id);

-- 仅在全部阅读事实迁出、可靠终态控制释放后移除旧历史列；剩余行为只承载未完成控制。
ALTER TABLE runs DROP COLUMN error_code;
ALTER TABLE runs DROP COLUMN error_message;
ALTER TABLE runs DROP COLUMN finished_at_ms;
ALTER TABLE child_tasks DROP COLUMN final_message_id;
ALTER TABLE child_tasks DROP COLUMN error_code;
ALTER TABLE child_tasks DROP COLUMN error_message;
ALTER TABLE child_tasks DROP COLUMN finished_at_ms;
