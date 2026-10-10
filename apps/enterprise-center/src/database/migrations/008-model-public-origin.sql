-- Up Migration
-- 未设置时沿用 CENTER_PUBLIC_ORIGIN，不改动既有默认模型或企业身份。
ALTER TABLE public.model_settings ADD COLUMN public_origin text;
ALTER TABLE public.model_settings ADD CONSTRAINT model_settings_public_origin CHECK (
  public_origin IS NULL OR (length(public_origin) > 0 AND octet_length(public_origin) <= 8192)
);

-- Down Migration
ALTER TABLE public.model_settings DROP COLUMN public_origin;
