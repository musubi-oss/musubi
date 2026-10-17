-- Skill rating per queue (plan 5.6): Elo / Glicko-2 / OpenSkill, seasons, decay.
create table if not exists musubi.rating_queues (
    id          text primary key,
    system      smallint not null default 0,         -- 0 elo, 1 glicko2, 2 openskill
    params      jsonb not null default '{}'::jsonb,
    season      int not null default 1,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.ratings (
    queue_id    text not null references musubi.rating_queues(id) on delete cascade,
    season      int not null,
    account_id  uuid not null,
    mu          double precision not null,
    sigma       double precision not null default 0,
    vol         double precision not null default 0,
    games       int not null default 0,
    wins        int not null default 0,
    losses      int not null default 0,
    draws       int not null default 0,
    last_played timestamptz,
    decayed_at  timestamptz,
    updated_at  timestamptz not null default now(),
    primary key (queue_id, season, account_id)
);
create index if not exists ratings_ladder on musubi.ratings (queue_id, season, mu desc);

create table if not exists musubi.rating_matches (
    id          uuid primary key,
    queue_id    text not null references musubi.rating_queues(id) on delete cascade,
    season      int not null,
    match_id    text,
    teams       jsonb not null,
    ranks       jsonb not null,
    deltas      jsonb not null,
    created_at  timestamptz not null default now()
);
create index if not exists rating_matches_queue on musubi.rating_matches (queue_id, created_at desc);
