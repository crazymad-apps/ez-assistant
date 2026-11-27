CREATE TABLE controller_prompt (
    singleton_key INTEGER PRIMARY KEY CHECK (singleton_key = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    content TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
) STRICT;

INSERT INTO controller_prompt (singleton_key, enabled, content, revision, updated_at_ms)
VALUES (1, 0, '', 0, 0);

CREATE TABLE controller_permission_grants (
    grant_id TEXT PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope = 'controller'),
    variants_json TEXT NOT NULL,
    matcher_json TEXT NOT NULL,
    max_uses INTEGER NOT NULL CHECK (max_uses > 0),
    remaining_uses INTEGER NOT NULL CHECK (remaining_uses >= 0 AND remaining_uses <= max_uses),
    expires_at_ms INTEGER CHECK (expires_at_ms IS NULL OR expires_at_ms >= 0),
    note TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    consumed_at_ms INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0)
) STRICT;

CREATE INDEX idx_controller_permission_grants_active
    ON controller_permission_grants (remaining_uses, created_at_ms, grant_id);
