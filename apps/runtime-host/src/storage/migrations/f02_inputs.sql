-- 复用 inputs 的队列与幂等身份；已结束输入只保留身份，全部历史字段归会话文件。
-- 外层迁移器持独占锁并临时关闭 FK；复制、校验及引用恢复均在同一版本事务内完成。
CREATE TABLE inputs_f02 (
    queue_order INTEGER PRIMARY KEY AUTOINCREMENT,
    priority_order INTEGER,
    input_id TEXT NOT NULL UNIQUE,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    idempotency_key TEXT,
    user_message_id TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('queued','committed')),
    input_kind TEXT NOT NULL DEFAULT 'message' CHECK(input_kind IN ('message','command')),
    command_json TEXT,
    command_result_json TEXT,
    queued_message_json TEXT,
    accepted_at_ms INTEGER,
    agent_variant TEXT DEFAULT 'build' CHECK(agent_variant IN ('plan','build')),
    origin TEXT DEFAULT 'user' CHECK(origin IN ('user','runtime')),
    goal_id TEXT,
    goal_generation INTEGER CHECK(goal_generation IS NULL OR goal_generation>0),
    goal_turn INTEGER CHECK(goal_turn IS NULL OR goal_turn>0),
    goal_reply_route_json TEXT,
    skill_activation_json TEXT,
    cross_session_json TEXT,
    channel_source_json TEXT,
    agent_shell_target TEXT CHECK(agent_shell_target IS NULL OR (input_kind='message' AND
        agent_shell_target IN ('windows_powershell_51','cmd','powershell_7','git_bash','posix_sh','bash','zsh'))),
    CHECK((goal_id IS NULL AND goal_generation IS NULL AND goal_turn IS NULL)
        OR (goal_id IS NOT NULL AND goal_generation IS NOT NULL AND goal_turn IS NOT NULL)),
    CHECK((
        (input_kind='message' AND command_json IS NULL AND command_result_json IS NULL
            AND ((user_message_id IS NOT NULL AND accepted_at_ms IS NOT NULL
                AND agent_variant IS NOT NULL AND origin IS NOT NULL)
                OR (state='committed' AND user_message_id IS NULL AND priority_order IS NULL
                    AND accepted_at_ms IS NULL AND agent_variant IS NULL AND origin IS NULL
                    AND queued_message_json IS NULL AND goal_id IS NULL
                    AND goal_generation IS NULL AND goal_turn IS NULL AND goal_reply_route_json IS NULL
                    AND skill_activation_json IS NULL AND cross_session_json IS NULL
                    AND channel_source_json IS NULL AND agent_shell_target IS NULL)))
        OR (input_kind='command' AND queued_message_json IS NULL AND command_result_json IS NULL
            AND goal_id IS NULL AND goal_generation IS NULL AND goal_turn IS NULL
            AND goal_reply_route_json IS NULL AND skill_activation_json IS NULL
            AND cross_session_json IS NULL AND channel_source_json IS NULL
            AND ((state='queued' AND command_json IS NOT NULL AND origin='runtime'
                    AND user_message_id IS NOT NULL AND accepted_at_ms IS NOT NULL
                    AND agent_variant IS NOT NULL)
                OR (state='committed' AND command_json IS NULL AND priority_order IS NULL
                    AND user_message_id IS NULL AND accepted_at_ms IS NULL
                    AND agent_variant IS NULL AND origin IS NULL)))
    ) IS TRUE),
    UNIQUE(session_id,idempotency_key)
);
WITH source AS (
    SELECT i.*, (i.state='committed' AND (i.input_kind='command' OR NOT EXISTS(
        SELECT 1 FROM runs r WHERE r.input_id=i.input_id AND r.session_id=i.session_id
            AND r.status IN ('accepted','running','cancelling')))) AS release
    FROM inputs i
)
INSERT INTO inputs_f02 SELECT queue_order,
    CASE WHEN release THEN NULL ELSE priority_order END,
    input_id,session_id,idempotency_key,
    CASE WHEN release THEN NULL ELSE user_message_id END,
    state,input_kind,
    CASE WHEN release THEN NULL ELSE command_json END,
    NULL,queued_message_json,
    CASE WHEN release THEN NULL ELSE accepted_at_ms END,
    CASE WHEN release THEN NULL ELSE agent_variant END,
    CASE WHEN release THEN NULL ELSE origin END,
    CASE WHEN release THEN NULL ELSE goal_id END,
    CASE WHEN release THEN NULL ELSE goal_generation END,
    CASE WHEN release THEN NULL ELSE goal_turn END,
    CASE WHEN release THEN NULL ELSE goal_reply_route_json END,
    CASE WHEN release THEN NULL ELSE skill_activation_json END,
    CASE WHEN release THEN NULL ELSE cross_session_json END,
    CASE WHEN release THEN NULL ELSE channel_source_json END,
    CASE WHEN release THEN NULL ELSE agent_shell_target END FROM source;
-- 原序号上限不能因清空过历史队列而回退；仅复制本输入表已存在的计数。
INSERT INTO sqlite_sequence(name,seq) SELECT 'inputs_f02',seq FROM sqlite_sequence
    WHERE name='inputs' AND NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='inputs_f02');
UPDATE sqlite_sequence SET seq=MAX(seq,COALESCE((SELECT seq FROM sqlite_sequence WHERE name='inputs'),0))
    WHERE name='inputs_f02';
DROP TABLE inputs;
ALTER TABLE inputs_f02 RENAME TO inputs;
