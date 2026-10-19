-- Archived game-server logs: one gzip NDJSON object per (server, snapshot) in object storage
create table if not exists musubi.server_log_archives (
    id          uuid primary key,
    server_id   uuid not null,
    fleet_id    uuid not null,
    key         text not null,
    bytes       bigint not null,
    lines       integer not null,
    from_at     timestamptz,
    to_at       timestamptz,
    reason      text not null default 'terminated',
    created_at  timestamptz not null default now()
);
create index if not exists server_log_archives_server_idx on musubi.server_log_archives (server_id, created_at desc);
create index if not exists server_log_archives_fleet_idx on musubi.server_log_archives (fleet_id, created_at desc);
