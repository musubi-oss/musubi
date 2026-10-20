-- Fleet aliases (GameLift Alias parity): a stable name clients/matchmakers allocate through, so
-- fleets can be swapped underneath (simple → fleet/pool) or retired with a message (terminal).
create table if not exists musubi.fleet_aliases (
    name         text primary key,
    description  text not null default '',
    routing      text not null check (routing in ('simple', 'terminal')),
    target       text,
    message      text not null default '',
    metadata     jsonb not null default '{}'::jsonb,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);
