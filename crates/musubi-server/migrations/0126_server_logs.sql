-- Game server log capture (competitive P0-9): stdout/stderr of provider-owned processes and
-- lines pushed through the game server SDK, per server, with retention.
create table if not exists musubi.server_logs (
    id        uuid primary key,
    server_id uuid not null,
    fleet_id  uuid not null,
    at        timestamptz not null default now(),
    source    text not null default 'stdout',
    level     text not null default 'info',
    message   text not null
);
create index if not exists server_logs_server_idx on musubi.server_logs (server_id, at desc);
create index if not exists server_logs_fleet_idx on musubi.server_logs (fleet_id, at desc);
