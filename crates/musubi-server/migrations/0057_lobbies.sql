-- Lobbies (plan 5.4): waiting rooms with team slots, passwords, listing and a one-call
-- transition to a match / dedicated server.
create table if not exists musubi.lobbies (
    id            uuid primary key,
    name          text not null,
    host_id       uuid not null references musubi.accounts(id) on delete cascade,
    visibility    text not null check (visibility in ('public', 'private')),
    code          text unique,
    password_hash text,
    max_members   integer not null,
    team_count    integer not null default 0,
    team_size     integer not null default 0,
    region        text,
    metadata      jsonb not null default '{}'::jsonb,
    state         text not null default 'waiting' check (state in ('waiting', 'started')),
    started       jsonb,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    heartbeat_at  timestamptz not null default now()
);
create index if not exists lobbies_list_idx on musubi.lobbies (visibility, state, created_at desc);
create index if not exists lobbies_metadata_idx on musubi.lobbies using gin (metadata);

create table if not exists musubi.lobby_members (
    lobby_id   uuid not null references musubi.lobbies(id) on delete cascade,
    account_id uuid not null references musubi.accounts(id) on delete cascade,
    username   text not null,
    team       integer not null default 0,
    ready      boolean not null default false,
    joined_at  timestamptz not null default now(),
    primary key (lobby_id, account_id)
);
-- one lobby per account at a time
create unique index if not exists lobby_members_account_idx on musubi.lobby_members (account_id);
