-- Up Migration
-- 设备归 Host 安装，申请人删除不删除设备；在线状态不持久化。
CREATE TABLE public.host_devices (
  id uuid PRIMARY KEY,
  installation_id uuid NOT NULL,
  credential_hash text NOT NULL CHECK (credential_hash ~ '^[0-9a-f]{64}$'),
  name text NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
  platform text NOT NULL CHECK (length(platform) BETWEEN 1 AND 32),
  host_version text NOT NULL CHECK (length(host_version) BETWEEN 1 AND 64),
  applicant_user_id integer NOT NULL,
  status text NOT NULL CHECK (status IN ('pending','registered','rejected','withdrawn','revoked')),
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE UNIQUE INDEX host_devices_active_installation ON public.host_devices(installation_id)
  WHERE status IN ('pending','registered');
CREATE INDEX host_devices_status_created ON public.host_devices(status,created_at DESC,id);
CREATE TABLE public.host_device_users (
  device_id uuid NOT NULL REFERENCES public.host_devices(id) ON DELETE CASCADE,
  user_id integer NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
  desktop_view boolean NOT NULL DEFAULT false,
  desktop_control boolean NOT NULL DEFAULT false,
  PRIMARY KEY(device_id,user_id),
  CHECK (NOT desktop_control OR desktop_view)
);
CREATE INDEX host_device_users_user ON public.host_device_users(user_id);

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
