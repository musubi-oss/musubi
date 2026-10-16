-- Plan 4.9 time machine: every shard snapshot is appended here as well (world_snapshots keeps only
-- the newest per node), so a world can be rebuilt as of any retained tick from the snapshot at or
-- before it plus the journal up to it. Rows age out by retention.world_history_days.
create table if not exists musubi.world_history (
    name      text        not null,
    node_id   uuid        not null,
    tick      bigint      not null,
    entities  jsonb       not null,
    taken_at  timestamptz not null default now(),
    primary key (name, node_id, tick)
);
create index if not exists world_history_taken_at on musubi.world_history (taken_at);
