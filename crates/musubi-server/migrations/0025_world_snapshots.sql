-- Persistent entities of every meshing world; a node adopting an orphaned world restores from here.
create table if not exists musubi.world_snapshots (
    name        text primary key references musubi.worlds(name) on delete cascade,
    entities    jsonb not null default '[]'::jsonb,
    tick        bigint not null default 0,
    updated_at  timestamptz not null default now()
);
