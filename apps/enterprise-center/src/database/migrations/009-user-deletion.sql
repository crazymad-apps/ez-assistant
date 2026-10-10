-- Up Migration
-- 审计身份是不可复用的历史 ID，不随账号删除级联或清空；LEFT JOIN 允许已删除用户。
ALTER TABLE public.management_audit DROP CONSTRAINT management_audit_actor_user_id_fkey;
ALTER TABLE public.management_audit DROP CONSTRAINT management_audit_target_user_id_fkey;

-- Down Migration
DO $$ BEGIN RAISE EXCEPTION 'Restore a verified matching backup to downgrade'; END $$;
