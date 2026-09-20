-- Up Migration
ALTER TABLE public.users ADD COLUMN upgrade_note text;
UPDATE public.users SET upgrade_note='before';
