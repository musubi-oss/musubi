-- Rewarded ads (competitive P1-53): placements with reward tables and per-player caps, ad views
-- the client opens before showing an ad, and rewards granted from network S2S callbacks.
create table if not exists musubi.ad_placements (
    id             text primary key,
    name           text not null default '',
    description    text not null default '',
    reward_table   text not null,
    daily_cap      integer not null default 0,
    cooldown_secs  integer not null default 0,
    platforms      text[] not null default '{}',
    trust_client   boolean not null default false,
    enabled        boolean not null default true,
    metadata       jsonb not null default '{}'::jsonb,
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now()
);

create table if not exists musubi.ad_views (
    id            uuid primary key,
    placement_id  text not null references musubi.ad_placements(id) on delete cascade,
    account_id    uuid not null,
    platform      text not null default '',
    status        text not null default 'started',
    reward_id     uuid,
    created_at    timestamptz not null default now(),
    completed_at  timestamptz
);
create index if not exists ad_views_account_idx on musubi.ad_views (account_id, created_at desc);

create table if not exists musubi.ad_rewards (
    id            uuid primary key,
    placement_id  text not null,
    account_id    uuid not null,
    provider      text not null,
    external_id   text not null,
    view_id       uuid,
    status        text not null,
    reason        text not null default '',
    roll          jsonb,
    created_at    timestamptz not null default now(),
    unique (provider, external_id)
);
create index if not exists ad_rewards_account_idx on musubi.ad_rewards (account_id, created_at desc);
create index if not exists ad_rewards_placement_idx on musubi.ad_rewards (placement_id, created_at desc);
