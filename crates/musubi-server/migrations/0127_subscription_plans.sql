-- Store-independent subscription plans (competitive P0-12): VIP / battle-pass-as-subscription paid
-- from the wallet or granted by the server, with trial, grace, pause and per-period rewards.
create table if not exists musubi.subscription_plans (
    id            text primary key,
    name          text not null,
    description   text not null default '',
    period_secs   bigint not null,
    grace_secs    bigint not null default 0,
    trial_secs    bigint not null default 0,
    price         jsonb,
    reward_table  text,
    features      text[] not null default '{}',
    metadata      jsonb not null default '{}'::jsonb,
    enabled       boolean not null default true,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);

create table if not exists musubi.subscriptions (
    id                    uuid primary key,
    account_id            uuid not null references musubi.accounts(id) on delete cascade,
    plan_id               text not null references musubi.subscription_plans(id) on delete cascade,
    status                text not null default 'active',
    source                text not null default 'wallet',
    started_at            bigint not null,
    current_period_end    bigint not null,
    grace_until           bigint,
    paused_remaining_secs bigint,
    cancel_at_period_end  boolean not null default false,
    periods               integer not null default 1,
    periods_left          integer not null default 0,
    metadata              jsonb not null default '{}'::jsonb,
    created_at            timestamptz not null default now(),
    updated_at            timestamptz not null default now()
);
create index if not exists subscriptions_account_idx on musubi.subscriptions (account_id, status);
create index if not exists subscriptions_due_idx on musubi.subscriptions (current_period_end)
    where status in ('trial', 'active');
create index if not exists subscriptions_grace_idx on musubi.subscriptions (grace_until)
    where status = 'grace';
