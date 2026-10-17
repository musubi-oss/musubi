-- Platform entitlements (plan 5.11): a platform purchase (Steam DLC appid, PSN entitlement label,
-- Xbox product id, …) maps to a reward table; each account receives it at most once.
create table if not exists musubi.entitlements (
    provider     text not null,
    external_id  text not null,
    reward_table text not null references musubi.reward_tables(id) on delete cascade,
    label        text not null default '',
    created_at   timestamptz not null default now(),
    primary key (provider, external_id)
);
create table if not exists musubi.entitlement_grants (
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    provider     text not null,
    external_id  text not null,
    granted_at   timestamptz not null default now(),
    revoked_at   timestamptz,
    primary key (account_id, provider, external_id)
);
