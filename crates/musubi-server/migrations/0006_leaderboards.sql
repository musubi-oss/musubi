-- Leaderboards. One definition row; records are keyed by (leaderboard, expiry window, owner) so a reset
-- schedule simply starts a new window without touching old rows.
create table if not exists musubi.leaderboards (
    id             text primary key,
    authoritative  boolean not null default false,
    sort_order     smallint not null default 1,          -- 0 asc, 1 desc
    operator       smallint not null default 0,          -- 0 best, 1 set, 2 incr, 3 decr
    reset_schedule text,                                 -- cron, null = never
    metadata       jsonb not null default '{}'::jsonb,
    created_at     timestamptz not null default now()
);

create table if not exists musubi.leaderboard_records (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    owner_id       uuid not null,
    username       text,
    score          bigint not null default 0,
    subscore       bigint not null default 0,
    num_score      integer not null default 1,
    metadata       jsonb not null default '{}'::jsonb,
    expiry_time    timestamptz not null default 'epoch',
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now(),
    primary key (leaderboard_id, expiry_time, owner_id)
);
create index if not exists leaderboard_records_rank_desc_idx on musubi.leaderboard_records (leaderboard_id, expiry_time, score desc, subscore desc, updated_at asc, owner_id);
create index if not exists leaderboard_records_rank_asc_idx on musubi.leaderboard_records (leaderboard_id, expiry_time, score asc, subscore asc, updated_at asc, owner_id);
create index if not exists leaderboard_records_owner_idx on musubi.leaderboard_records (owner_id, leaderboard_id, expiry_time desc);
