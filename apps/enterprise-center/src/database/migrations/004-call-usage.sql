-- Up Migration
-- 用量与正文采集独立；旧调用没有可追溯的 usage。
ALTER TABLE public.llm_recording_settings
  ADD COLUMN usage_available_from timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP;
ALTER TABLE public.llm_calls
  ADD COLUMN input_tokens bigint CHECK (input_tokens >= 0),
  ADD COLUMN output_tokens bigint CHECK (output_tokens >= 0),
  ADD COLUMN total_tokens bigint CHECK (total_tokens >= 0),
  ADD COLUMN cached_input_tokens bigint CHECK (cached_input_tokens >= 0),
  ADD COLUMN reasoning_tokens bigint CHECK (reasoning_tokens >= 0),
  ADD COLUMN usage_state text NOT NULL DEFAULT 'unavailable'
    CHECK (usage_state IN ('pending', 'available', 'partial', 'unavailable')),
  ADD COLUMN usage_reason text DEFAULT 'historical';

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
