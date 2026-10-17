-- group treasury (plan 5.4): a wallet per group and currency with its own ledger
create table if not exists musubi.group_wallets (
    group_id   uuid not null references musubi.groups(id) on delete cascade,
    currency   text not null references musubi.currencies(code) on delete restrict,
    balance    bigint not null default 0 check (balance >= 0),
    updated_at timestamptz not null default now(),
    primary key (group_id, currency)
);
create table if not exists musubi.group_wallet_ledger (
    id            uuid primary key,
    group_id      uuid not null references musubi.groups(id) on delete cascade,
    account_id    uuid,
    currency      text not null,
    delta         bigint not null,
    balance_after bigint not null,
    reason        text not null default '',
    created_at    timestamptz not null default now()
);
create index if not exists group_wallet_ledger_group_idx on musubi.group_wallet_ledger (group_id, created_at desc);
