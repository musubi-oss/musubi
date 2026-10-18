-- Player statistics (5.6): typed counters per account, sliced by an optional scope
-- (mode, season, map …), each with a visibility so profiles can show some and hide others.
create table if not exists musubi.stat_defs (
    id          text primary key,
    name        text not null default '',
    -- how an update folds into the stored value: sum | max | min | last
    kind        text not null default 'sum',
    -- who may read it: public | friends | private (owner and server only)
    visibility  text not null default 'public',
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.player_stats (
    account_id  uuid not null,
    stat_id     text not null references musubi.stat_defs(id) on delete cascade,
    -- '' = all-time / global slice
    scope       text not null default '',
    value       double precision not null default 0,
    -- number of updates folded in (lets a client show averages)
    samples     bigint not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (account_id, stat_id, scope)
);

create index if not exists player_stats_top on musubi.player_stats (stat_id, scope, value desc);
