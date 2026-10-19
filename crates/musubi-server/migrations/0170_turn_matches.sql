-- Asynchronous turn-based matches: correspondence-style games where players move on their own schedule
create table if not exists musubi.turn_matches (
    id             uuid primary key,
    game           text not null,
    state          text not null default 'open',
    visibility     text not null default 'public',
    code           text,
    created_by     uuid not null,
    players        jsonb not null default '[]'::jsonb,
    min_players    integer not null default 2,
    max_players    integer not null default 2,
    turn           integer not null default 0,
    current_seat   integer,
    deadline_at    timestamptz,
    turn_secs      bigint not null default 86400,
    timeout_action text not null default 'skip',
    max_skips      integer not null default 3,
    auto_start     boolean not null default true,
    board          jsonb not null default 'null'::jsonb,
    version        bigint not null default 0,
    metadata       jsonb not null default '{}'::jsonb,
    winners        jsonb not null default '[]'::jsonb,
    result         jsonb not null default 'null'::jsonb,
    rematch_of     uuid,
    created_at     timestamptz not null default now(),
    updated_at     timestamptz not null default now(),
    started_at     timestamptz,
    finished_at    timestamptz
);
create index if not exists turn_matches_open_idx
    on musubi.turn_matches (game, created_at) where state = 'open';
create index if not exists turn_matches_deadline_idx
    on musubi.turn_matches (deadline_at) where state = 'active';
create index if not exists turn_matches_players_idx
    on musubi.turn_matches using gin (players jsonb_path_ops);
create unique index if not exists turn_matches_code_idx
    on musubi.turn_matches (code) where code is not null and state = 'open';

create table if not exists musubi.turn_moves (
    match_id   uuid not null references musubi.turn_matches(id) on delete cascade,
    turn       integer not null,
    seat       integer not null,
    account_id uuid,
    kind       text not null,
    data       jsonb not null default 'null'::jsonb,
    created_at timestamptz not null default now(),
    primary key (match_id, turn)
);
