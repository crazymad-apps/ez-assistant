-- Up Migration
-- 旧连接保持关闭；启用周期随 Provider 持久化，不建立响应 ID 或 Token 表。
ALTER TABLE public.providers ADD COLUMN server_history_key text;
UPDATE public.providers SET connection = connection ||
  '{"server_history_enabled":false,"server_history_support_override":null}'::jsonb;
ALTER TABLE public.providers ADD CONSTRAINT providers_server_history_policy CHECK ((
  jsonb_typeof(connection->'server_history_enabled') = 'boolean'
  AND connection ? 'server_history_support_override'
  AND (connection->'server_history_support_override' = 'null'::jsonb
    OR connection->>'server_history_support_override' IN ('supported','unsupported'))
  AND ((connection->>'server_history_enabled' = 'false' AND server_history_key IS NULL)
    OR (connection->>'server_history_enabled' = 'true' AND length(server_history_key) > 0))
) IS TRUE);

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
