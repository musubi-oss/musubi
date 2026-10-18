create table if not exists musubi.group_challenges (
    id uuid primary key,
    name text not null,
    kind text not null default 'challenge',
    metric text not null default 'score',
    groups uuid[] not null default '{}',
    starts_at timestamptz not null default now(),
    ends_at timestamptz not null,
    rewards jsonb not null default '{}'::jsonb,
    metadata jsonb not null default '{}'::jsonb,
    settled_at timestamptz,
    winner uuid,
    created_at timestamptz not null default now()
);
create index if not exists group_challenges_window_idx on musubi.group_challenges (ends_at, settled_at);

create table if not exists musubi.group_challenge_scores (
    challenge_id uuid not null references musubi.group_challenges (id) on delete cascade,
    group_id uuid not null,
    account_id uuid not null,
    score bigint not null default 0,
    updated_at timestamptz not null default now(),
    primary key (challenge_id, group_id, account_id)
);
create index if not exists group_challenge_scores_group_idx on musubi.group_challenge_scores (challenge_id, group_id);
