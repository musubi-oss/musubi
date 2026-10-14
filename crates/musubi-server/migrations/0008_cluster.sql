-- Node registry: every process heartbeats its row; peers consider a node gone after `dead_after`.
create table if not exists musubi.nodes (
    id           uuid primary key,
    name         text not null,
    region       text not null default '',
    roles        text[] not null default '{}',
    version      text not null default '',
    advertise    text not null default '',
    started_at   timestamptz not null default now(),
    heartbeat_at timestamptz not null default now(),
    metadata     jsonb not null default '{}'::jsonb
);
create index if not exists nodes_heartbeat_idx on musubi.nodes (heartbeat_at desc);
