-- Player server browser: favourites, join history and reputation keyed by the server's address:port.
create table if not exists musubi.server_favorites (
    account_id  uuid not null,
    key         text not null,
    name        text not null default '',
    added_at    timestamptz not null default now(),
    primary key (account_id, key)
);
create table if not exists musubi.server_history (
    account_id     uuid not null,
    key            text not null,
    name           text not null default '',
    fleet_id       uuid,
    server_id      uuid,
    joins          integer not null default 1,
    last_joined_at timestamptz not null default now(),
    primary key (account_id, key)
);
create table if not exists musubi.server_ratings (
    account_id  uuid not null,
    key         text not null,
    score       smallint not null check (score between 1 and 5),
    tags        text[] not null default '{}',
    at          timestamptz not null default now(),
    primary key (account_id, key)
);
create index if not exists server_ratings_key_idx on musubi.server_ratings (key);
