-- Up Migration
-- Agent 配置是中心权威数据；M0 仅预建结构，管理与依赖诊断由后续里程碑接入。
CREATE TABLE public.custom_agents (
  name text PRIMARY KEY,
  description text NOT NULL,
  prompt text NOT NULL,
  skills jsonb NOT NULL CHECK (jsonb_typeof(skills) = 'array'),
  mcp jsonb NOT NULL CHECK (jsonb_typeof(mcp) = 'array'),
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 插件的展示与上架状态独立于归档版本；首次上传默认未上架。
CREATE TABLE public.plugins (
  name text PRIMARY KEY,
  display_name text NOT NULL,
  description text NOT NULL,
  publication text NOT NULL DEFAULT 'unpublished'
    CHECK (publication IN ('unpublished', 'listed', 'deprecated')),
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- 联合主键只保留每版本当前完整包，不建立平台子包或同版本替换历史。
CREATE TABLE public.plugin_packages (
  plugin_name text NOT NULL REFERENCES public.plugins(name),
  version text NOT NULL,
  manifest jsonb NOT NULL CHECK (jsonb_typeof(manifest) = 'object'),
  archive_format text NOT NULL CHECK (archive_format IN ('zip', 'tar_gzip')),
  sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
  size_bytes bigint NOT NULL CHECK (size_bytes > 0 AND size_bytes <= 2147483648),
  created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (plugin_name, version)
);

-- Down Migration
-- 回退必须恢复已验证的数据库与插件根备份，不自动删除用户数据。
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
