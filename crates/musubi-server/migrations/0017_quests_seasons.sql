create table if not exists musubi.quests (
    id           text primary key,
    name         text not null default '',
    description  text not null default '',
    kind         text not null default 'daily',
    objectives   jsonb not null default '[]'::jsonb,
    rewards      text,
    enabled      boolean not null default true
);
create table if not exists musubi.quest_progress (
    account_id    uuid not null,
    quest_id      text not null,
    period_start  timestamptz not null,
    progress      jsonb not null default '{}'::jsonb,
    completed_at  timestamptz,
    claimed_at    timestamptz,
    primary key (account_id, quest_id, period_start)
);
create table if not exists musubi.seasons (
    id         text primary key,
    name       text not null default '',
    starts_at  timestamptz not null,
    ends_at    timestamptz not null,
    track_id   text not null,
    levels     jsonb not null default '[]'::jsonb
);
create table if not exists musubi.season_state (
    account_id  uuid not null,
    season_id   text not null,
    premium     boolean not null default false,
    claimed     jsonb not null default '[]'::jsonb,
    primary key (account_id, season_id)
);
