-- P1-46: several allocations per game server process (`spec.slots_per_server`).
alter table musubi.game_servers add column if not exists slots_used integer not null default 0;
