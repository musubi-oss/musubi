-- Limited grants (plan 5.11): a cap on how much of a currency / item / reward table any single
-- account may receive from server-side grants per window — a backstop against a buggy module or
-- a leaked server key duplicating rewards.
create table if not exists musubi.grant_limits (
    kind            text not null,
    key             text not null,
    window_secs     integer not null,
    max_per_account bigint not null,
    primary key (kind, key)
);
create table if not exists musubi.grant_usage (
    id         uuid primary key,
    account_id uuid not null,
    kind       text not null,
    key        text not null,
    amount     bigint not null,
    at         timestamptz not null default now()
);
create index if not exists grant_usage_lookup_idx on musubi.grant_usage (account_id, kind, key, at desc);
