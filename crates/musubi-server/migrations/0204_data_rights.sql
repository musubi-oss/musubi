-- Asynchronous data-rights requests (Pragma data-rights polling parity): players file export /
-- erase requests, the leader processes them on a schedule, results are kept for a while.
create table if not exists musubi.data_rights_requests (
    id            uuid primary key,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    kind          text not null check (kind in ('export', 'erase')),
    status        text not null default 'pending'
                  check (status in ('pending', 'running', 'done', 'failed', 'cancelled')),
    requested_by  text not null default 'player',
    include_game_data boolean not null default true,
    requested_at  timestamptz not null default now(),
    started_at    timestamptz,
    completed_at  timestamptz,
    expires_at    timestamptz,
    error         text,
    result        jsonb
);

create index if not exists data_rights_requests_status_idx
    on musubi.data_rights_requests (status, requested_at);
create index if not exists data_rights_requests_account_idx
    on musubi.data_rights_requests (account_id, requested_at desc);
