-- Up Migration
CREATE TABLE public.organizations (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  code varchar(64) NOT NULL UNIQUE CHECK (code ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
  name varchar(64) NOT NULL CHECK (length(btrim(name)) > 0),
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE public.organization_members (
  organization_id uuid NOT NULL REFERENCES public.organizations(id),
  user_id integer NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
  role text NOT NULL CHECK (role IN ('member', 'admin')),
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (organization_id, user_id)
);
CREATE INDEX organization_members_user_idx ON public.organization_members (user_id, organization_id);
ALTER TABLE public.center_identity ADD COLUMN legacy_organization_id uuid REFERENCES public.organizations(id);
WITH legacy AS (
  INSERT INTO public.organizations(code, name) VALUES ('default', '默认组织') RETURNING id
)
UPDATE public.center_identity SET legacy_organization_id = legacy.id FROM legacy;
ALTER TABLE public.center_identity ALTER COLUMN legacy_organization_id SET NOT NULL;

ALTER TABLE public.users DROP CONSTRAINT users_role_check;
UPDATE public.users SET role = 'system_admin' WHERE role = 'admin';
ALTER TABLE public.users ADD CONSTRAINT users_role_check CHECK (role IN ('user', 'system_admin'));

-- 组织管理员由系统管理员显式指定。账号停用状态独立保留。
INSERT INTO public.organization_members(organization_id, user_id, role)
SELECT center.legacy_organization_id, account.id, 'member'
FROM public.center_identity center CROSS JOIN public.users account;

ALTER TABLE public.management_audit ADD COLUMN organization_id uuid REFERENCES public.organizations(id);
