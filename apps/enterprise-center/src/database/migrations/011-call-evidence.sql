-- Up Migration
-- 历史关闭/开启只映射 off/all，不自动扩大采集；历史观察为空，不回读正文猜测成功。
ALTER TABLE public.llm_recording_settings ADD COLUMN mode text NOT NULL DEFAULT 'off'
  CHECK (mode IN ('off','anomalies','all'));
UPDATE public.llm_recording_settings SET mode = CASE WHEN enabled THEN 'all' ELSE 'off' END;
ALTER TABLE public.llm_recording_settings DROP COLUMN enabled;
ALTER TABLE public.llm_calls
  ADD COLUMN response_observation text CHECK (response_observation IN ('pending','normal','anomalous','unconfirmed')),
  ADD COLUMN response_observation_reason text,
  ADD COLUMN response_source text CHECK (response_source IN ('upstream_http','websocket_bridge','websocket_compact')),
  ADD COLUMN response_content_type text,
  ADD COLUMN response_content_encoding text,
  ADD COLUMN upstream_failure_snapshot jsonb CHECK (jsonb_typeof(upstream_failure_snapshot) = 'object'),
  DROP CONSTRAINT llm_calls_request_snapshot_state_check,
  DROP CONSTRAINT llm_calls_response_snapshot_state_check;
ALTER TABLE public.llm_calls
  ADD CONSTRAINT llm_calls_request_snapshot_state_check CHECK (request_snapshot_state IN ('disabled','pending','complete','partial','failed','expired','missing','not_received')),
  ADD CONSTRAINT llm_calls_response_snapshot_state_check CHECK (response_snapshot_state IN ('disabled','pending','complete','partial','failed','expired','missing','not_received'));

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
