-- Player statistics depth: constraints, cycles, community (global) aggregates, client-writable
-- stats, AVGRATE windows and idempotent updates.
alter table musubi.stat_defs add column if not exists min_value double precision;
alter table musubi.stat_defs add column if not exists max_value double precision;
alter table musubi.stat_defs add column if not exists max_change double precision;
alter table musubi.stat_defs add column if not exists increment_only boolean not null default false;
alter table musubi.stat_defs add column if not exists default_value double precision not null default 0;
alter table musubi.stat_defs add column if not exists cycles jsonb not null default '[]'::jsonb;
alter table musubi.stat_defs add column if not exists global boolean not null default false;
alter table musubi.stat_defs add column if not exists client_writable boolean not null default false;
alter table musubi.stat_defs add column if not exists window_secs bigint not null default 0;
alter table musubi.stat_defs add column if not exists metadata jsonb not null default '{}'::jsonb;

alter table musubi.player_stats add column if not exists num double precision not null default 0;
alter table musubi.player_stats add column if not exists den double precision not null default 0;

create table if not exists musubi.global_stats (
    stat_id     text not null references musubi.stat_defs(id) on delete cascade,
    scope       text not null default '',
    value       double precision not null default 0,
    samples     bigint not null default 0,
    num         double precision not null default 0,
    den         double precision not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (stat_id, scope)
);

create table if not exists musubi.stat_transactions (
    id          text primary key,
    created_at  timestamptz not null default now()
);
