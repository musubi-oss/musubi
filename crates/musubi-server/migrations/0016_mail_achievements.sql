create table if not exists musubi.mail (
    id          uuid primary key,
    account_id  uuid not null,
    title       text not null,
    body        text not null default '',
    metadata    jsonb not null default '{}'::jsonb,
    rewards     text,
    read_at     timestamptz,
    claimed_at  timestamptz,
    expires_at  timestamptz,
    created_at  timestamptz not null default now()
);
create index if not exists mail_account_idx on musubi.mail (account_id, created_at desc);

create table if not exists musubi.achievements (
    id           text primary key,
    name         text not null default '',
    description  text not null default '',
    target       int not null default 1,
    rewards      text,
    hidden       boolean not null default false,
    enabled      boolean not null default true
);
create table if not exists musubi.achievement_progress (
    account_id      uuid not null,
    achievement_id  text not null,
    progress        int not null default 0,
    unlocked_at     timestamptz,
    primary key (account_id, achievement_id)
);
