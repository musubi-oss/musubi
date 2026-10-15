create table if not exists musubi.progression_tracks (
    id      text primary key,
    name    text not null default '',
    levels  jsonb not null default '[]'::jsonb
);
create table if not exists musubi.progress (
    account_id  uuid not null,
    track_id    text not null,
    xp          bigint not null default 0,
    level       int not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (account_id, track_id)
);
create table if not exists musubi.energies (
    id             text primary key,
    name           text not null default '',
    max            int not null,
    regen_seconds  bigint not null,
    initial        int not null default 0
);
create table if not exists musubi.energy_state (
    account_id  uuid not null,
    energy_id   text not null,
    amount      int not null,
    updated_at  timestamptz not null default now(),
    primary key (account_id, energy_id)
);
create table if not exists musubi.streaks (
    id              text primary key,
    name            text not null default '',
    period_seconds  bigint not null default 86400,
    grace_seconds   bigint not null default 86400,
    milestones      jsonb not null default '[]'::jsonb
);
create table if not exists musubi.streak_state (
    account_id  uuid not null,
    streak_id   text not null,
    count       int not null default 0,
    last_claim  timestamptz,
    primary key (account_id, streak_id)
);
