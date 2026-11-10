-- Up Migration
-- 旧中心资源以稳定默认组织接收，迁移后业务写入必须显式指定组织。
ALTER TABLE public.custom_agents ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
ALTER TABLE public.plugins ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
ALTER TABLE public.plugin_packages ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
ALTER TABLE public.feedback ADD COLUMN organization_id uuid REFERENCES public.organizations(id);

UPDATE public.custom_agents SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
UPDATE public.plugins SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
UPDATE public.plugin_packages SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
UPDATE public.feedback SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);

ALTER TABLE public.plugin_packages DROP CONSTRAINT plugin_packages_plugin_name_fkey;
ALTER TABLE public.plugins DROP CONSTRAINT plugins_pkey,
  ADD PRIMARY KEY (organization_id, name);
ALTER TABLE public.plugin_packages DROP CONSTRAINT plugin_packages_pkey,
  ADD PRIMARY KEY (organization_id, plugin_name, version),
  ADD FOREIGN KEY (organization_id, plugin_name) REFERENCES public.plugins(organization_id, name);
ALTER TABLE public.custom_agents DROP CONSTRAINT custom_agents_pkey,
  ADD PRIMARY KEY (organization_id, name);
ALTER TABLE public.feedback ALTER COLUMN organization_id SET NOT NULL,
  DROP CONSTRAINT feedback_owner_user_id_submission_id_key,
  ADD UNIQUE (organization_id, owner_user_id, submission_id);
CREATE INDEX feedback_organization_time ON public.feedback(organization_id, created_at DESC, id DESC);

ALTER TABLE public.providers ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
ALTER TABLE public.llm_calls ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
UPDATE public.providers SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
UPDATE public.llm_calls SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
ALTER TABLE public.providers ALTER COLUMN organization_id SET NOT NULL;
ALTER TABLE public.llm_calls ALTER COLUMN organization_id SET NOT NULL;
CREATE INDEX providers_organization ON public.providers(organization_id, provider_instance_id);
CREATE INDEX llm_calls_organization_time ON public.llm_calls(organization_id, started_at DESC, id DESC);
ALTER TABLE public.llm_calls ADD COLUMN continuation_digest text CHECK (continuation_digest ~ '^[0-9a-f]{64}$');
CREATE INDEX llm_calls_organization_continuation ON public.llm_calls(organization_id, continuation_digest)
  WHERE continuation_digest IS NOT NULL;
CREATE INDEX llm_calls_organization_user_time ON public.llm_calls(organization_id, user_id, started_at DESC);

ALTER TABLE public.center_identity ADD COLUMN public_origin text CHECK (
  public_origin IS NULL OR (length(public_origin) > 0 AND octet_length(public_origin) <= 8192)
);
UPDATE public.center_identity SET public_origin = (SELECT public_origin FROM public.model_settings);
ALTER TABLE public.model_settings ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
UPDATE public.model_settings SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
ALTER TABLE public.model_settings DROP COLUMN public_origin, DROP COLUMN singleton, ADD PRIMARY KEY (organization_id);

ALTER TABLE public.llm_recording_settings ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
UPDATE public.llm_recording_settings SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
ALTER TABLE public.llm_recording_settings DROP COLUMN singleton, ADD PRIMARY KEY (organization_id);
INSERT INTO public.llm_recording_settings(organization_id)
SELECT id FROM public.organizations ON CONFLICT (organization_id) DO NOTHING;

ALTER TABLE public.host_devices ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
UPDATE public.host_devices SET organization_id = (SELECT legacy_organization_id FROM public.center_identity);
ALTER TABLE public.host_devices ALTER COLUMN organization_id SET NOT NULL, ADD UNIQUE (id, organization_id);
CREATE INDEX host_devices_organization_time ON public.host_devices(organization_id, status, created_at DESC, id);
ALTER TABLE public.host_device_users ADD COLUMN organization_id uuid;
UPDATE public.host_device_users g SET organization_id = d.organization_id FROM public.host_devices d WHERE d.id = g.device_id;
-- M0 中已移除或已停用的成员不能在升级后恢复旧授权。
DELETE FROM public.host_device_users g WHERE NOT EXISTS (
  SELECT 1 FROM public.organization_members m JOIN public.users u ON u.id = m.user_id
  WHERE m.organization_id = g.organization_id AND m.user_id = g.user_id AND m.enabled AND u.enabled
);
ALTER TABLE public.host_device_users ALTER COLUMN organization_id SET NOT NULL,
  DROP CONSTRAINT host_device_users_device_id_fkey,
  ADD FOREIGN KEY (device_id, organization_id) REFERENCES public.host_devices(id, organization_id) ON DELETE CASCADE,
  ADD FOREIGN KEY (organization_id, user_id) REFERENCES public.organization_members(organization_id, user_id) ON DELETE CASCADE;
CREATE INDEX host_device_users_member ON public.host_device_users(organization_id, user_id);

-- 只迁移有明确旧组织归属的动作，未知及全局配置动作继续留在系统审计。
UPDATE public.management_audit SET organization_id = (SELECT legacy_organization_id FROM public.center_identity)
WHERE organization_id IS NULL AND action IN (
  'agent_created', 'agent_updated', 'agent_deleted',
  'plugin_updated', 'plugin_publication_changed', 'plugin_package_uploaded',
  'feedback_created', 'feedback_handled',
  'model_provider_created', 'model_provider_updated', 'model_provider_deleted',
  'model_catalog_refreshed', 'model_configuration_saved', 'model_configuration_reset', 'model_default_changed',
  'llm_recording_settings_changed', 'llm_snapshot_viewed',
  'device_applied', 'device_managed', 'device_withdrawn', 'device_revoked', 'device_user_login'
);
ALTER TABLE public.management_audit ADD COLUMN scope text;
UPDATE public.management_audit SET scope = CASE WHEN organization_id IS NULL THEN 'system' ELSE 'organization' END;
ALTER TABLE public.management_audit ALTER COLUMN scope SET NOT NULL, ADD CHECK (
  (scope = 'system' AND organization_id IS NULL) OR (scope = 'organization' AND organization_id IS NOT NULL)
);
CREATE INDEX management_audit_organization_time ON public.management_audit(organization_id, occurred_at DESC, id DESC);

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
