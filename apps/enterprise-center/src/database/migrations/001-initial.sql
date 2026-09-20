-- Up Migration
-- 首次安装与升级共用框架入口，版本记录由 node-pg-migrate 管理。
CREATE TABLE public.center_identity (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  center_id uuid NOT NULL UNIQUE,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE public.users (
  id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  username varchar(64) NOT NULL UNIQUE CHECK (username ~ '^[a-z0-9._-]{3,64}$'),
  display_name varchar(64) NOT NULL CHECK (length(btrim(display_name)) > 0),
  role text NOT NULL CHECK (role IN ('user', 'admin')),
  is_super_admin boolean NOT NULL DEFAULT false,
  enabled boolean NOT NULL DEFAULT true,
  password_hash text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE public.management_audit (
  id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  occurred_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  actor_user_id integer REFERENCES public.users(id),
  target_user_id integer REFERENCES public.users(id),
  action text NOT NULL,
  success boolean NOT NULL,
  reason_code text,
  request_id uuid NOT NULL,
  details jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(details) = 'object')
);
CREATE INDEX management_audit_time_idx ON public.management_audit(occurred_at, id);

-- 初始数据与建表、框架版本记录同事务；已执行的文件不会在重启或后续升级时重跑。
INSERT INTO public.center_identity(singleton,center_id) VALUES (true,gen_random_uuid());

-- 用户指定默认凭据 admin / 123456。这里只保存预生成的 scrypt 摘要，运行期不另设初始化流程。
WITH administrator AS (
  INSERT INTO public.users(username,display_name,role,is_super_admin,enabled,password_hash)
  VALUES ('admin','超级管理员','admin',true,true,
    'scrypt$131072$8$1$3f50b71f30dab7ca864439210dec64c9$0eae34c42bfe0b9c49dc953e561ad132c578e19f5429d29dae7b26e983c065e4')
  RETURNING id
)
INSERT INTO public.management_audit(actor_user_id,target_user_id,action,success,request_id)
SELECT id,id,'center_initialized',true,gen_random_uuid() FROM administrator;
