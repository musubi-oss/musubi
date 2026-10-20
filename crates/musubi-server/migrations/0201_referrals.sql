-- Referral programs (LootLocker parity): per-program invite codes, referrer/referee rewards,
-- milestone rewards for the referrer, void by the operator.
create table if not exists musubi.referral_programs (
    id                           text primary key,
    name                         text not null default '',
    enabled                      boolean not null default true,
    referee_reward_table         text,
    referrer_reward_table        text,
    milestones                   jsonb not null default '[]'::jsonb,
    max_referrals_per_player     integer,
    referee_max_account_age_days integer,
    starts_at                    timestamptz,
    ends_at                      timestamptz,
    metadata                     jsonb not null default '{}'::jsonb,
    created_at                   timestamptz not null default now(),
    updated_at                   timestamptz not null default now()
);

create table if not exists musubi.referral_codes (
    program_id  text not null references musubi.referral_programs(id) on delete cascade,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    code        text not null,
    created_at  timestamptz not null default now(),
    primary key (program_id, account_id),
    unique (program_id, code)
);

create table if not exists musubi.referrals (
    id          uuid primary key,
    program_id  text not null references musubi.referral_programs(id) on delete cascade,
    referrer_id uuid not null references musubi.accounts(id) on delete cascade,
    referee_id  uuid not null references musubi.accounts(id) on delete cascade,
    code        text not null,
    status      text not null default 'accepted' check (status in ('accepted', 'voided')),
    void_reason text,
    created_at  timestamptz not null default now(),
    voided_at   timestamptz,
    unique (program_id, referee_id)
);

create index if not exists referrals_program_referrer_idx
    on musubi.referrals (program_id, referrer_id, status);
