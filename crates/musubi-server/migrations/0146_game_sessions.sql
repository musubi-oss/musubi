-- P1-16/56: game session objects (dedicated allocation, authoritative match or custom connect info)
-- with properties/data, join policy + invite codes, per-player seat reservations and templates
-- players may instantiate themselves.
create table if not exists musubi.game_sessions (
    id              uuid primary key,
    name            text not null default '',
    kind            text not null,
    state           text not null default 'active',
    owner_id        uuid,
    template_id     text,
    fleet_id        uuid,
    server_id       uuid,
    match_id        text,
    region          text not null default '',
    connect         jsonb not null default '{}'::jsonb,
    properties      jsonb not null default '{}'::jsonb,
    data            jsonb not null default '{}'::jsonb,
    max_players     int not null default 0,
    join_policy     text not null default 'open',
    join_code       text,
    idempotency_key text,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    terminated_at   timestamptz
);
create unique index if not exists game_sessions_idem on musubi.game_sessions (idempotency_key) where idempotency_key is not null;
create unique index if not exists game_sessions_code on musubi.game_sessions (join_code) where join_code is not null and state <> 'terminated';
create index if not exists game_sessions_server on musubi.game_sessions (server_id) where server_id is not null;
create index if not exists game_sessions_match on musubi.game_sessions (match_id) where match_id is not null;
create index if not exists game_sessions_owner on musubi.game_sessions (owner_id) where owner_id is not null;
create index if not exists game_sessions_props on musubi.game_sessions using gin (properties);
create index if not exists game_sessions_state on musubi.game_sessions (state, join_policy, region);

create table if not exists musubi.game_session_members (
    session_id  uuid not null references musubi.game_sessions(id) on delete cascade,
    account_id  uuid not null,
    state       text not null default 'reserved',
    token       text not null,
    reserved_at timestamptz not null default now(),
    joined_at   timestamptz,
    left_at     timestamptz,
    primary key (session_id, account_id)
);
create index if not exists game_session_members_account on musubi.game_session_members (account_id);

create table if not exists musubi.game_session_templates (
    id                   text primary key,
    name                 text not null default '',
    fleet                text,
    region               text,
    module               text,
    params               jsonb not null default '{}'::jsonb,
    max_players          int not null default 0,
    join_policy          text not null default 'invite',
    max_active_per_owner int not null default 1,
    wait_secs            int not null default 0,
    enabled              boolean not null default true,
    created_at           timestamptz not null default now(),
    updated_at           timestamptz not null default now()
);
