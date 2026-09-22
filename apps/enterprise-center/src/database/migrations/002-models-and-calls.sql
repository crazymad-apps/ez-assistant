-- Up Migration
-- 连接、目录、固定值分别保存；在线刷新不得修改固定参数或模板文件。
CREATE TABLE public.providers (
  provider_instance_id uuid PRIMARY KEY,
  connection jsonb NOT NULL CHECK (jsonb_typeof(connection) = 'object'),
  api_key text NOT NULL DEFAULT '',
  model_catalog jsonb CHECK (jsonb_typeof(model_catalog) = 'array'),
  catalog_refreshed_at timestamptz,
  catalog_connection_changed boolean NOT NULL DEFAULT false,
  CHECK ((model_catalog IS NULL) = (catalog_refreshed_at IS NULL)),
  CHECK (model_catalog IS NOT NULL OR NOT catalog_connection_changed)
);
CREATE TABLE public.model_fixed_configs (
  provider_instance_id uuid NOT NULL REFERENCES public.providers(provider_instance_id) ON DELETE CASCADE,
  model_id text NOT NULL CHECK (length(model_id) > 0),
  origin text NOT NULL CHECK (origin IN ('online', 'manual')),
  parameters jsonb NOT NULL CHECK (jsonb_typeof(parameters) = 'object'),
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (provider_instance_id, model_id)
);
-- 默认引用在 Provider 删除后仍保留，供界面明确显示失效原因。
CREATE TABLE public.model_settings (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  provider_instance_id uuid,
  model_id text,
  CHECK ((provider_instance_id IS NULL) = (model_id IS NULL))
);
INSERT INTO public.model_settings(singleton) VALUES (true);
CREATE TABLE public.llm_recording_settings (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  enabled boolean NOT NULL DEFAULT false,
  content_retention_days integer NOT NULL DEFAULT 7,
  index_retention_days integer NOT NULL DEFAULT 90,
  CHECK (content_retention_days BETWEEN 1 AND index_retention_days AND index_retention_days <= 3650)
);
INSERT INTO public.llm_recording_settings(singleton) VALUES (true);
CREATE TABLE public.llm_calls (
  id uuid PRIMARY KEY,
  user_id integer,
  username text,
  provider_instance_id uuid,
  provider_name text,
  model_id text,
  kind text NOT NULL CHECK (kind IN ('proxy', 'admin_test')),
  protocol text NOT NULL CHECK (protocol IN ('open_ai_responses', 'open_ai_chat_completions')),
  started_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  ended_at timestamptz,
  http_status integer CHECK (http_status BETWEEN 100 AND 599),
  reason text,
  outcome text NOT NULL CHECK (outcome IN ('in_progress', 'forwarded', 'rejected', 'upstream_error', 'interrupted', 'unknown')),
  request_snapshot_state text NOT NULL CHECK (request_snapshot_state IN ('disabled','pending','complete','partial','failed','expired','missing')),
  response_snapshot_state text NOT NULL CHECK (response_snapshot_state IN ('disabled','pending','complete','partial','failed','expired','missing')),
  request_path text, response_path text,
  request_bytes bigint CHECK (request_bytes >= 0), response_bytes bigint CHECK (response_bytes >= 0),
  request_sha256 text, response_sha256 text,
  request_snapshot_reason text, response_snapshot_reason text
);
CREATE INDEX llm_calls_started_id ON public.llm_calls(started_at DESC, id DESC);
CREATE INDEX llm_calls_user_started ON public.llm_calls(user_id, started_at DESC);

-- Down Migration
-- 旧程序必须拒绝新账本；回退通过独立备份恢复，不自动删除业务数据。
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
