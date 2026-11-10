-- Up Migration
-- 旧默认成为唯一允许项；保留已失效引用，供管理员核对并处理。
ALTER TABLE public.model_settings
  ADD COLUMN allow_member_selection boolean NOT NULL DEFAULT false,
  ADD COLUMN revision uuid;

CREATE TABLE public.organization_models (
  organization_id uuid NOT NULL REFERENCES public.organizations(id),
  -- Provider 删除后仍须展示失效的策略引用，因此不依赖 Provider 外键。
  provider_instance_id uuid NOT NULL,
  model_id text NOT NULL,
  PRIMARY KEY (organization_id, provider_instance_id, model_id)
);

INSERT INTO public.organization_models (organization_id, provider_instance_id, model_id)
SELECT organization_id, provider_instance_id, model_id FROM public.model_settings
WHERE provider_instance_id IS NOT NULL AND model_id IS NOT NULL;

-- 策略保存可在一个事务内替换集合和默认，提交时验证默认仍在集合。
ALTER TABLE public.model_settings ADD CONSTRAINT model_settings_allowed_default
  FOREIGN KEY (organization_id, provider_instance_id, model_id)
  REFERENCES public.organization_models(organization_id, provider_instance_id, model_id)
  DEFERRABLE INITIALLY DEFERRED;

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
