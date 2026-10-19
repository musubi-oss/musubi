-- Per-server counters/lists (Agones Counters & Lists model) and indexes for the cross-fleet server inventory.
alter table musubi.game_servers add column if not exists counters jsonb not null default '{}'::jsonb;
alter table musubi.game_servers add column if not exists lists jsonb not null default '{}'::jsonb;
create index if not exists game_servers_labels_gin on musubi.game_servers using gin (labels);
create index if not exists game_servers_state_created_idx on musubi.game_servers (state, created_at desc);
