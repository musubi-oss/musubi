-- Quest pools (challenge rotations with randomized goal pools), player-created challenges from
-- templates, and streak depth (calendar periods, local time, decay, caps, repeating rewards).
create table if not exists musubi.quest_pools (
    id                  text primary key,
    name                text not null default '',
    description         text not null default '',
    kind                text not null default 'daily',
    period_seconds      bigint not null default 86400,
    reset_hour          int not null default 0,
    local_time          boolean not null default false,
    pick                int not null default 3,
    goals               jsonb not null default '[]'::jsonb,
    rerolls_per_period  int not null default 1,
    generation          int not null default 0,
    starts_at           timestamptz,
    ends_at             timestamptz,
    enabled             boolean not null default true,
    metadata            jsonb not null default '{}'::jsonb,
    updated_at          timestamptz not null default now()
);

create table if not exists musubi.quest_assignments (
    account_id    uuid not null,
    pool_id       text not null references musubi.quest_pools(id) on delete cascade,
    period_key    text not null,
    slot          int not null,
    goal_id       text not null,
    progress      jsonb not null default '{}'::jsonb,
    completed_at  timestamptz,
    claimed_at    timestamptz,
    rerolled      int not null default 0,
    assigned_at   timestamptz not null default now(),
    primary key (account_id, pool_id, period_key, slot)
);
create index if not exists quest_assignments_pool_idx on musubi.quest_assignments (pool_id, period_key);

create table if not exists musubi.challenge_templates (
    id                text primary key,
    name              text not null default '',
    description       text not null default '',
    objectives        jsonb not null default '[]'::jsonb,
    duration_secs     bigint not null default 86400,
    max_participants  int not null default 8,
    rewards_by_rank   jsonb not null default '{}'::jsonb,
    enabled           boolean not null default true,
    metadata          jsonb not null default '{}'::jsonb
);

create table if not exists musubi.challenges (
    id                uuid primary key,
    template_id       text not null,
    owner_id          uuid not null,
    name              text not null default '',
    open              boolean not null default false,
    join_code         text unique,
    max_participants  int not null default 8,
    starts_at         timestamptz not null,
    ends_at           timestamptz not null,
    state             text not null default 'active',
    created_at        timestamptz not null default now()
);
create index if not exists challenges_state_idx on musubi.challenges (state, ends_at);

create table if not exists musubi.challenge_participants (
    challenge_id  uuid not null references musubi.challenges(id) on delete cascade,
    account_id    uuid not null,
    score         bigint not null default 0,
    progress      jsonb not null default '{}'::jsonb,
    rank          int,
    claimed_at    timestamptz,
    joined_at     timestamptz not null default now(),
    primary key (challenge_id, account_id)
);
create index if not exists challenge_participants_account_idx on musubi.challenge_participants (account_id);

create table if not exists musubi.challenge_invites (
    challenge_id  uuid not null references musubi.challenges(id) on delete cascade,
    account_id    uuid not null,
    invited_by    uuid not null,
    created_at    timestamptz not null default now(),
    primary key (challenge_id, account_id)
);

alter table musubi.streaks add column if not exists calendar boolean not null default false;
alter table musubi.streaks add column if not exists reset_hour int not null default 0;
alter table musubi.streaks add column if not exists local_time boolean not null default false;
alter table musubi.streaks add column if not exists max_count int not null default 0;
alter table musubi.streaks add column if not exists decay int not null default 0;
alter table musubi.streaks add column if not exists repeat_every int not null default 0;
alter table musubi.streaks add column if not exists repeat_rewards text;
alter table musubi.streaks add column if not exists enabled boolean not null default true;
alter table musubi.streak_state add column if not exists claims int not null default 0;
alter table musubi.streak_state add column if not exists best int not null default 0;
alter table musubi.streak_state add column if not exists missed int not null default 0;
