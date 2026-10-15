-- Resume records also carry world subscriptions and party memberships.
alter table musubi.resumable_sessions add column if not exists worlds  jsonb not null default '[]'::jsonb;
alter table musubi.resumable_sessions add column if not exists parties jsonb not null default '[]'::jsonb;
