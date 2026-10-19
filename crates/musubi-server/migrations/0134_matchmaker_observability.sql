-- Matchmaking observability (competitive P0-58): per-ticket decision traces and formed matches.
create table if not exists musubi.matchmaker_ticket_log (
    ticket      text primary key,
    queue       text,
    accounts    uuid[] not null default '{}',
    outcome     text not null,
    wait_ms     bigint not null default 0,
    events      jsonb not null default '[]'::jsonb,
    finished_at timestamptz not null default now()
);
create index if not exists matchmaker_ticket_log_time_idx on musubi.matchmaker_ticket_log (finished_at desc);
create index if not exists matchmaker_ticket_log_accounts_idx on musubi.matchmaker_ticket_log using gin (accounts);

create table if not exists musubi.matchmaker_matches (
    token        text primary key,
    queue        text,
    region       text,
    quality      real,
    team_mmr_gap double precision,
    tickets      text[] not null default '{}',
    accounts     uuid[] not null default '{}',
    wait_ms      bigint[] not null default '{}',
    created_at   timestamptz not null default now()
);
create index if not exists matchmaker_matches_time_idx on musubi.matchmaker_matches (created_at desc);
create index if not exists matchmaker_matches_accounts_idx on musubi.matchmaker_matches using gin (accounts);
