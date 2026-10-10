-- Up Migration
CREATE TABLE public.desktop_releases (
  version text PRIMARY KEY,
  notes text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE public.desktop_release_targets (
  version text NOT NULL REFERENCES public.desktop_releases(version),
  target text NOT NULL CHECK (target IN ('windows-x86_64', 'windows-aarch64', 'darwin-aarch64')),
  manifest_text text NOT NULL,
  manifest_signature text NOT NULL,
  manifest_sha256 text NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
  publication text NOT NULL DEFAULT 'draft' CHECK (publication IN ('draft', 'published', 'withdrawn')),
  first_published_at timestamptz,
  PRIMARY KEY (version, target),
  CHECK ((publication = 'draft') = (first_published_at IS NULL))
);

-- Down Migration
-- 已发布更新记录不得由应用自动回退；按完整独立备份恢复。
SELECT 1;
