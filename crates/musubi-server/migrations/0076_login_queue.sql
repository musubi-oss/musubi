-- Login queue (plan 5.1): logins past the online capacity wait here in arrival order.
create table if not exists musubi.login_queue (
    id          uuid primary key,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    created_at  timestamptz not null default now(),
    polled_at   timestamptz not null default now(),
    admitted_at timestamptz
);
create index if not exists login_queue_waiting_idx on musubi.login_queue (created_at) where admitted_at is null;
