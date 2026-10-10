-- Up Migration
-- 历史身份与目标不设删除联动外键；users.id 不可复用。
CREATE TABLE public.feedback (
  id uuid PRIMARY KEY,
  owner_user_id integer NOT NULL CHECK (owner_user_id > 0),
  owner_username text NOT NULL,
  owner_display_name text NOT NULL,
  submission_id uuid NOT NULL,
  source text NOT NULL CHECK (source IN ('manual','agent')),
  target_kind text NOT NULL CHECK (target_kind IN ('platform','plugin','knowledge')),
  target_reference text,
  target_name text NOT NULL CHECK (char_length(target_name) BETWEEN 1 AND 120),
  target_version text,
  target_content_ref text,
  target_location text,
  title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 120),
  description text NOT NULL CHECK (char_length(description) BETWEEN 1 AND 4000),
  reproduction text,
  expected_result text,
  environment jsonb NOT NULL CHECK (jsonb_typeof(environment) = 'object'),
  status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','processing','resolved','closed')),
  handling_note text,
  handler_user_id integer CHECK (handler_user_id > 0),
  handler_username text,
  handler_display_name text,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (owner_user_id, submission_id),
  CHECK ((target_kind = 'knowledge' OR target_reference IS NOT NULL)
    AND (target_kind <> 'platform' OR (target_reference = 'ez-assistant' AND target_name = 'EZ Assistant'))
    AND (target_kind = 'plugin' OR target_version IS NULL)),
  CHECK (target_reference IS NULL OR char_length(target_reference) BETWEEN 1 AND 512),
  CHECK (target_version IS NULL OR char_length(target_version) BETWEEN 1 AND 64),
  CHECK (target_content_ref IS NULL OR char_length(target_content_ref) BETWEEN 1 AND 512),
  CHECK (target_location IS NULL OR char_length(target_location) BETWEEN 1 AND 512),
  CHECK (reproduction IS NULL OR char_length(reproduction) BETWEEN 1 AND 2000),
  CHECK (expected_result IS NULL OR char_length(expected_result) BETWEEN 1 AND 2000),
  CHECK (handling_note IS NULL OR char_length(handling_note) BETWEEN 1 AND 2000),
  CHECK (status NOT IN ('resolved','closed') OR handling_note IS NOT NULL),
  CHECK ((handler_user_id IS NULL AND handler_username IS NULL AND handler_display_name IS NULL)
    OR (handler_user_id IS NOT NULL AND handler_username IS NOT NULL AND handler_display_name IS NOT NULL))
);
CREATE INDEX feedback_owner_time ON public.feedback (owner_user_id, created_at DESC, id DESC);
CREATE INDEX feedback_status_time ON public.feedback (status, created_at DESC, id DESC);
CREATE INDEX feedback_target_time ON public.feedback (target_kind, target_reference, created_at DESC, id DESC);

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
