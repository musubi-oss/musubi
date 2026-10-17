-- Fleet build pipeline (plan 5.9): versioned builds with canary share and promote/rollback,
-- plus server lifecycle timestamps for vCPU-hour accounting.
create table if not exists musubi.fleet_builds (
    fleet_id       uuid not null references musubi.fleets(id) on delete cascade,
    version        text not null,
    spec           jsonb not null default '{}'::jsonb,
    state          text not null default 'canary' check (state in ('active', 'canary', 'retired')),
    canary_percent integer not null default 0 check (canary_percent between 0 and 100),
    canary_regions text[] not null default '{}',
    notes          text not null default '',
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now(),
    primary key (fleet_id, version)
);
alter table musubi.game_servers add column if not exists build_version text;
alter table musubi.game_servers add column if not exists ready_at timestamptz;
alter table musubi.game_servers add column if not exists terminated_at timestamptz;
