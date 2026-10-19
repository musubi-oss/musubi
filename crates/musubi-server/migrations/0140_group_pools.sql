-- Group economies (competitive P1-54): contribution pools with tiers, member donation requests,
-- and a team store paid from the treasury.
create table if not exists musubi.group_pools (
    id                       uuid primary key,
    group_id                 uuid not null references musubi.groups(id) on delete cascade,
    name                     text not null,
    currency                 text not null,
    goal                     bigint not null,
    raised                   bigint not null default 0,
    tiers                    jsonb not null default '[]'::jsonb,
    contributor_reward_table text,
    status                   text not null default 'open',
    ends_at                  timestamptz,
    created_by               uuid not null,
    created_at               timestamptz not null default now(),
    completed_at             timestamptz
);
create index if not exists group_pools_group_idx on musubi.group_pools (group_id, status);

create table if not exists musubi.group_pool_contributions (
    id         uuid primary key,
    pool_id    uuid not null references musubi.group_pools(id) on delete cascade,
    account_id uuid not null,
    amount     bigint not null,
    at         timestamptz not null default now()
);
create index if not exists group_pool_contributions_pool_idx on musubi.group_pool_contributions (pool_id, account_id);

create table if not exists musubi.group_pool_tier_grants (
    pool_id    uuid not null references musubi.group_pools(id) on delete cascade,
    tier       integer not null,
    account_id uuid not null,
    at         timestamptz not null default now(),
    primary key (pool_id, tier, account_id)
);

create table if not exists musubi.group_donation_requests (
    id           uuid primary key,
    group_id     uuid not null references musubi.groups(id) on delete cascade,
    account_id   uuid not null,
    currency     text not null,
    amount       bigint not null,
    note         text not null default '',
    status       text not null default 'open',
    fulfilled_by uuid,
    created_at   timestamptz not null default now(),
    closed_at    timestamptz
);
create index if not exists group_donation_requests_group_idx on musubi.group_donation_requests (group_id, status);

create table if not exists musubi.group_store (
    group_id   uuid not null references musubi.groups(id) on delete cascade,
    item_id    text not null,
    currency   text not null,
    price      bigint not null,
    quantity   bigint not null default 1,
    enabled    boolean not null default true,
    updated_at timestamptz not null default now(),
    primary key (group_id, item_id)
);
