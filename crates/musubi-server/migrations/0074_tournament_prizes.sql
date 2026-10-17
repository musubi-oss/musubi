-- Tournament prizes (plan 5.6): rank bands → reward table, paid out by mail when a window ends.
create table if not exists musubi.tournament_prizes (
    tournament_id text not null references musubi.leaderboards(id) on delete cascade,
    from_rank     integer not null check (from_rank >= 1),
    to_rank       integer not null check (to_rank >= from_rank),
    reward_table  text not null,
    title         text not null default '',
    body          text not null default '',
    primary key (tournament_id, from_rank)
);
-- one payout per (tournament, window): the sweep claims it before sending anything
create table if not exists musubi.tournament_payouts (
    tournament_id text not null,
    expiry_time   timestamptz not null,
    paid          integer not null default 0,
    created_at    timestamptz not null default now(),
    primary key (tournament_id, expiry_time)
);
