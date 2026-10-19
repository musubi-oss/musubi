-- Native LiveOps (competitive P0-17/52/53): feature flags with targeting, A/B experiments with
-- sticky assignment and outcomes, scheduled live events with phases. Zanshin stays an optional
-- overlay (its answers are merged for names musubi does not define).
create table if not exists musubi.flags (
    name          text primary key,
    description   text not null default '',
    enabled       boolean not null default true,
    default_value jsonb not null default 'false'::jsonb,
    rules         jsonb not null default '[]'::jsonb,
    updated_at    timestamptz not null default now()
);

create table if not exists musubi.experiments (
    id              text primary key,
    name            text not null,
    description     text not null default '',
    status          text not null default 'draft',
    audience        text,
    rollout_pct     integer not null default 100,
    variants        jsonb not null default '[]'::jsonb,
    starts_at       bigint,
    ends_at         bigint,
    exclusion_group text,
    metadata        jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);

create table if not exists musubi.experiment_assignments (
    experiment_id text not null references musubi.experiments(id) on delete cascade,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    variant       text not null,
    assigned_at   timestamptz not null default now(),
    primary key (experiment_id, account_id)
);
create index if not exists experiment_assignments_account_idx on musubi.experiment_assignments (account_id);

create table if not exists musubi.experiment_outcomes (
    id            uuid primary key,
    experiment_id text not null references musubi.experiments(id) on delete cascade,
    account_id    uuid not null,
    variant       text not null,
    metric        text not null,
    value         double precision not null,
    created_at    timestamptz not null default now()
);
create index if not exists experiment_outcomes_idx on musubi.experiment_outcomes (experiment_id, metric, variant);

create table if not exists musubi.live_events (
    id              text primary key,
    name            text not null,
    description     text not null default '',
    enabled         boolean not null default true,
    phases          jsonb not null default '[]'::jsonb,
    starts_at       bigint not null,
    ends_at         bigint,
    recurrence_secs bigint,
    local_time      boolean not null default false,
    audience        text,
    metadata        jsonb not null default '{}'::jsonb,
    updated_at      timestamptz not null default now()
);
