-- Private tournaments (join code) and end-of-window hooks fired once per finished window.
alter table musubi.leaderboards add column if not exists join_code text;
create table if not exists musubi.leaderboard_ends (
    leaderboard_id text not null references musubi.leaderboards(id) on delete cascade,
    expiry_time    timestamptz not null,
    fired_at       timestamptz not null default now(),
    primary key (leaderboard_id, expiry_time)
);
