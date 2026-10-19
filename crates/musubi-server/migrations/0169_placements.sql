-- Queued game-session placements: allocation requests that wait for capacity (GameLift placement parity)
create table if not exists musubi.session_placements (
    id           uuid primary key,
    pool         text not null,
    status       text not null default 'pending',
    request      jsonb not null,
    metadata     jsonb not null default '{}'::jsonb,
    attempts     integer not null default 0,
    last_error   text,
    result       jsonb,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    timeout_at   timestamptz not null,
    fulfilled_at timestamptz
);
create index if not exists session_placements_pending_idx
    on musubi.session_placements (created_at) where status = 'pending';
create index if not exists session_placements_pool_idx
    on musubi.session_placements (pool, created_at desc);
