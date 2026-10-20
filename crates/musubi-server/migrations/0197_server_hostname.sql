-- Stable network identity (Agones headless-Service parity): a DNS-style name per game server,
-- `<short-id>.<fleet>.<dns_zone>`, assigned at creation when the fleet sets spec.dns_zone.
alter table musubi.game_servers add column if not exists hostname text;
create unique index if not exists game_servers_hostname_idx on musubi.game_servers (hostname) where hostname is not null;
