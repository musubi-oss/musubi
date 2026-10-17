-- Persistent entities of hibernated cells that were unloaded from memory (plan 4.9 scale-to-zero):
-- one row per cold cell, read back when interest, a lease or a spawn reaches the cell again.
create table if not exists musubi.world_cold_cells (
    name        text        not null,
    x           int         not null,
    y           int         not null,
    entities    jsonb       not null,
    tick        bigint      not null,
    unloaded_at timestamptz not null default now(),
    primary key (name, x, y)
);
