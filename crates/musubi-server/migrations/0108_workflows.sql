-- Durable workflows: long-running server logic as module steps with persisted state.
create table if not exists musubi.workflows (
    id           uuid primary key,
    name         text not null,
    key          text,
    status       text not null default 'running',
    step         integer not null default 0,
    attempt      integer not null default 0,
    max_attempts integer not null default 5,
    max_steps    integer not null default 10000,
    input        jsonb not null default '{}'::jsonb,
    state        jsonb not null default '{}'::jsonb,
    output       jsonb,
    error        text,
    signals      jsonb not null default '[]'::jsonb,
    history      jsonb not null default '[]'::jsonb,
    wake_at      timestamptz,
    account_id   uuid,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    finished_at  timestamptz
);
create unique index if not exists workflows_name_key_idx on musubi.workflows (name, key) where key is not null;
create index if not exists workflows_due_idx on musubi.workflows (wake_at) where status = 'running';
create index if not exists workflows_name_idx on musubi.workflows (name, created_at desc);
create index if not exists workflows_account_idx on musubi.workflows (account_id, created_at desc) where account_id is not null;
