alter table musubi.achievements add column if not exists platform_ids jsonb not null default '{}'::jsonb;
