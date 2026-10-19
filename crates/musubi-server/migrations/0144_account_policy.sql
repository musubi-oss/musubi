-- Account policy: per-field validation + edit cadence rules, dormant-account policy, per-field
-- edit history and dormancy bookkeeping.
create table if not exists musubi.account_policy (
    id          int primary key default 1 check (id = 1),
    policy      jsonb not null default '{}'::jsonb,
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.account_field_edits (
    account_id  uuid not null,
    field       text not null,
    changed_at  timestamptz not null default now(),
    primary key (account_id, field)
);

create table if not exists musubi.account_dormancy (
    account_id  uuid primary key,
    noticed_at  timestamptz,
    actioned_at timestamptz,
    action      text
);
