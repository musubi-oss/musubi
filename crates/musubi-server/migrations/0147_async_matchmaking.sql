-- Asynchronous (offline) PvP matchmaking: opponent picks from the rating ladder, attacks with
-- results, attack shields, opt-out, revenge.
create table if not exists musubi.async_mm_rules (
    queue_id    text primary key,
    rules       jsonb not null,
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.async_mm_shields (
    account_id  uuid not null,
    queue_id    text not null,
    until       timestamptz not null,
    source      text not null default 'server',
    primary key (account_id, queue_id)
);

create table if not exists musubi.async_mm_optouts (
    account_id  uuid not null,
    queue_id    text not null,
    created_at  timestamptz not null default now(),
    primary key (account_id, queue_id)
);

create table if not exists musubi.async_mm_attacks (
    id          uuid primary key,
    queue_id    text not null,
    attacker    uuid not null,
    defender    uuid not null,
    state       text not null default 'open' check (state in ('open', 'resolved', 'expired')),
    result      text check (result in ('win', 'loss', 'draw')),
    revenge_of  uuid,
    data        jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    expires_at  timestamptz not null,
    resolved_at timestamptz
);
create index if not exists async_mm_attacks_attacker on musubi.async_mm_attacks (queue_id, attacker, created_at desc);
create index if not exists async_mm_attacks_defender on musubi.async_mm_attacks (queue_id, defender, created_at desc);
create index if not exists async_mm_attacks_open on musubi.async_mm_attacks (queue_id, expires_at) where state = 'open';
create unique index if not exists async_mm_attacks_revenge on musubi.async_mm_attacks (revenge_of) where revenge_of is not null;
