-- Up Migration
UPDATE public.users SET upgrade_note='after' WHERE upgrade_note='before';
