-- Bucketed leaderboards and leagues (plan 5.6): a record lives in a (tier, bucket) scope; ranks and
-- pages are computed inside the scope. Tiers persist across windows and move at rollover.
alter table musubi.leaderboards add column if not exists bucket_size int not null default 0;  -- 0 = one global scope
alter table musubi.leaderboards add column if not exists league jsonb;                        -- {tiers, promote_pct, demote_pct}
alter table musubi.leaderboard_records add column if not exists bucket int not null default 0;
alter table musubi.leaderboard_records add column if not exists tier smallint not null default 0;
create index if not exists leaderboard_records_scope_idx
    on musubi.leaderboard_records (leaderboard_id, expiry_time, tier, bucket);

create table if not exists musubi.leaderboard_tiers (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    owner_id       uuid not null,
    tier           smallint not null default 0,
    updated_at     timestamptz not null default now(),
    primary key (leaderboard_id, owner_id)
);

create table if not exists musubi.leaderboard_rollovers (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    expiry_time    timestamptz not null,
    promoted       int not null default 0,
    demoted        int not null default 0,
    done_at        timestamptz not null default now(),
    primary key (leaderboard_id, expiry_time)
);
