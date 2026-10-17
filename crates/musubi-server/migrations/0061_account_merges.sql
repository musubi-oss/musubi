-- Account merge journal (plan 5.1): which account absorbed which, with what was moved.
create table if not exists musubi.account_merges (
    id         uuid primary key,
    target_id  uuid not null references musubi.accounts(id) on delete cascade,
    source_id  uuid not null,
    report     jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);
create index if not exists account_merges_target_idx on musubi.account_merges (target_id, created_at desc);
