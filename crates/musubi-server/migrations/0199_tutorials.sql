-- Tutorial sequences with per-player step tracking (Hiro parity).
create table if not exists musubi.tutorials (
    id            text primary key,
    title         text not null default '',
    start_step    integer not null default 0,
    max_step      integer not null,
    reward_table  text,
    labels        text[] not null default '{}',
    enabled       boolean not null default true,
    metadata      jsonb not null default '{}'::jsonb,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    check (max_step >= start_step)
);

create table if not exists musubi.tutorial_progress (
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    tutorial_id  text not null references musubi.tutorials(id) on delete cascade,
    state        text not null check (state in ('accepted', 'declined', 'abandoned', 'completed', 'reset')),
    step         integer not null default 0,
    resets       integer not null default 0,
    started_at   timestamptz not null default now(),
    completed_at timestamptz,
    updated_at   timestamptz not null default now(),
    primary key (account_id, tutorial_id)
);

create index if not exists tutorial_progress_tutorial_state_idx
    on musubi.tutorial_progress (tutorial_id, state, updated_at desc);
