-- per-key rate limits and per-account overrides / daily budgets
alter table musubi.server_keys
    add column if not exists per_second integer,
    add column if not exists burst integer,
    add column if not exists daily bigint;

create table if not exists musubi.account_limits (
    account_id uuid primary key references musubi.accounts(id) on delete cascade,
    http_per_second integer,
    http_burst integer,
    rpc_per_second integer,
    rpc_burst integer,
    daily bigint,
    note text not null default '',
    updated_at timestamptz not null default now()
);
