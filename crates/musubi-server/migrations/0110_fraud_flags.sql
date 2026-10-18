create table if not exists musubi.fraud_flags (
    id          uuid primary key,
    account_id  uuid not null,
    rule        text not null,
    count       bigint not null,
    "limit"     bigint not null,
    blocked     boolean not null default false,
    created_at  timestamptz not null default now(),
    resolved_at timestamptz
);
create index if not exists fraud_flags_open_idx
    on musubi.fraud_flags (account_id, rule) where resolved_at is null;
create index if not exists fraud_flags_created_idx on musubi.fraud_flags (created_at desc);
