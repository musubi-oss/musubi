-- Race-to-target cohorts on bucketed leaderboards (Hiro score-based event leaderboards).
alter table musubi.leaderboards add column if not exists race jsonb;
create table if not exists musubi.leaderboard_cohorts (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    expiry_time    timestamptz not null,
    tier           smallint not null,
    bucket         integer not null,
    opened_at      timestamptz not null default now(),
    closed_at      timestamptz,
    finishers      integer not null default 0,
    primary key (leaderboard_id, expiry_time, tier, bucket)
);
create table if not exists musubi.leaderboard_rolls (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    expiry_time    timestamptz not null,
    owner_id       uuid not null,
    rolls          integer not null default 0,
    last_roll_at   timestamptz not null default now(),
    primary key (leaderboard_id, expiry_time, owner_id)
);
