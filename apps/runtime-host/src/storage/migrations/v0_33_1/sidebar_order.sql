ALTER TABLE workspaces ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
ALTER TABLE session_resources ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN pin_order INTEGER NOT NULL DEFAULT 0;

WITH ranked AS MATERIALIZED (
    SELECT workspace_id, ROW_NUMBER() OVER (ORDER BY created_at_ms, workspace_id DESC) * 1024 AS value
    FROM workspaces
)
UPDATE workspaces SET sort_order = ranked.value FROM ranked
WHERE workspaces.workspace_id = ranked.workspace_id;

WITH ranked AS MATERIALIZED (
    SELECT r.session_id, ROW_NUMBER() OVER (
        PARTITION BY r.workspace_id ORDER BY s.created_at_ms, r.session_id DESC
    ) * 1024 AS value
    FROM session_resources r JOIN sessions s USING(session_id)
)
UPDATE session_resources SET sort_order = ranked.value FROM ranked
WHERE session_resources.session_id = ranked.session_id;

WITH ranked AS MATERIALIZED (
    SELECT session_id, ROW_NUMBER() OVER (ORDER BY updated_at_ms, session_id DESC) * 1024 AS value
    FROM sessions WHERE is_pinned = 1
)
UPDATE sessions SET pin_order = ranked.value FROM ranked WHERE sessions.session_id = ranked.session_id;

CREATE INDEX workspaces_sort_order ON workspaces(sort_order DESC, workspace_id);
-- 原索引仅用于按 workspace_id 查询；新索引保留此前缀。
DROP INDEX session_resources_workspace;
CREATE INDEX session_resources_workspace_order ON session_resources(workspace_id, sort_order DESC, session_id);
CREATE INDEX sessions_pin_order ON sessions(pin_order DESC, session_id) WHERE is_pinned = 1;
CREATE INDEX sessions_created_order ON sessions(created_at_ms DESC, session_id);
