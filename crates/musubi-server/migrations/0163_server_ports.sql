-- P0-7: extra named host ports per game server ({name: host_port}), next to the primary `port`.
alter table musubi.game_servers add column if not exists ports jsonb not null default '{}'::jsonb;
