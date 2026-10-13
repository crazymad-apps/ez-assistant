-- Up Migration
-- 原生状态与响应续接共用配置周期；关闭续接不会清除原生状态的隔离绑定。
ALTER TABLE public.providers DROP CONSTRAINT providers_server_history_policy;
UPDATE public.providers SET server_history_key = gen_random_uuid()::text WHERE server_history_key IS NULL;
ALTER TABLE public.providers ADD CONSTRAINT providers_server_history_policy CHECK ((
  jsonb_typeof(connection->'server_history_enabled') = 'boolean'
  AND connection ? 'server_history_support_override'
  AND (connection->'server_history_support_override' = 'null'::jsonb
    OR connection->>'server_history_support_override' IN ('supported','unsupported'))
  AND length(server_history_key) > 0
) IS TRUE);
UPDATE public.model_fixed_configs SET parameters =
  '{"server_compaction_enabled":false,"server_auto_compaction_enabled":false,"server_compaction_support":"unknown","input_token_count_support":"unknown"}'::jsonb || parameters;
ALTER TABLE public.llm_calls DROP CONSTRAINT llm_calls_kind_check;
ALTER TABLE public.llm_calls ADD CONSTRAINT llm_calls_kind_check
  CHECK (kind IN ('proxy','admin_test','context_compaction','input_tokens'));

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
