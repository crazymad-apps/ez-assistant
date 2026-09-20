-- Up Migration
ALTER TABLE public.users ADD COLUMN upgrade_note text;
UPDATE public.users SET display_name='must rollback';
