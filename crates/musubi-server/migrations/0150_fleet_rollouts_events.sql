-- Build rollouts (rolling / recreate replacement of running servers) and the per-fleet lifecycle event log.
create table if not exists musubi.fleet_rollouts (
    id              uuid primary key,
    fleet_id        uuid not null references musubi.fleets(id) on delete cascade,
    from_version    text,
    to_version      text not null,
    strategy        text not null,
    max_surge       integer not null default 1,
    max_unavailable integer not null default 1,
    state           text not null default 'running' check (state in ('running', 'paused', 'done', 'rolled_back', 'superseded')),
    replaced        integer not null default 0,
    failed_starts   integer not null default 0,
    note            text not null default '',
    started_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    finished_at     timestamptz
);
create index if not exists fleet_rollouts_fleet_idx on musubi.fleet_rollouts (fleet_id, started_at desc);

create table if not exists musubi.fleet_events (
    id          bigserial primary key,
    fleet_id    uuid not null,
    server_id   uuid,
    code        text not null,
    message     text not null default '',
    data        jsonb not null default '{}'::jsonb,
    at          timestamptz not null default now(),
    dispatched  boolean not null default false
);
create index if not exists fleet_events_fleet_idx on musubi.fleet_events (fleet_id, id desc);
create index if not exists fleet_events_undispatched_idx on musubi.fleet_events (id) where not dispatched;
