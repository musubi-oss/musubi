-- Redemption codes / key campaigns (competitive P1-52): campaigns with generated single-use or
-- shared multi-use codes that grant a reward table and/or hand out third-party platform keys.
create table if not exists musubi.code_campaigns (
    id              text primary key,
    name            text not null,
    reward_table    text,
    key_group       text,
    per_account     integer not null default 1,
    max_redemptions bigint,
    starts_at       timestamptz,
    ends_at         timestamptz,
    audience        text,
    enabled         boolean not null default true,
    metadata        jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);

create table if not exists musubi.codes (
    code        text primary key,
    campaign_id text not null references musubi.code_campaigns(id) on delete cascade,
    max_uses    bigint,
    uses        bigint not null default 0,
    revoked     boolean not null default false,
    batch       text not null default '',
    created_at  timestamptz not null default now()
);
create index if not exists codes_campaign_idx on musubi.codes (campaign_id, batch);

create table if not exists musubi.code_redemptions (
    id          uuid primary key,
    code        text not null,
    campaign_id text not null,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    reward      jsonb not null default '[]'::jsonb,
    keys        text[] not null default '{}',
    at          timestamptz not null default now()
);
create index if not exists code_redemptions_campaign_idx on musubi.code_redemptions (campaign_id, account_id);
create index if not exists code_redemptions_account_idx on musubi.code_redemptions (account_id, at desc);

create table if not exists musubi.key_groups (
    id         text primary key,
    name       text not null,
    platform   text not null default '',
    created_at timestamptz not null default now()
);

create table if not exists musubi.platform_keys (
    id         uuid primary key,
    group_id   text not null references musubi.key_groups(id) on delete cascade,
    key        text not null,
    claimed_by uuid,
    claimed_at timestamptz,
    unique (group_id, key)
);
create index if not exists platform_keys_free_idx on musubi.platform_keys (group_id) where claimed_by is null;
