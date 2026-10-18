-- Reward modifiers: time-bounded multipliers on XP, currency rewards and reward tables,
-- global (account_id null) or per account.
create table if not exists musubi.reward_modifiers (
    id         text primary key,
    account_id uuid,
    scope      text not null,
    factor     double precision not null,
    starts_at  timestamptz,
    ends_at    timestamptz,
    enabled    boolean not null default true,
    note       text not null default '',
    created_at timestamptz not null default now()
);
create index if not exists reward_modifiers_account_idx on musubi.reward_modifiers (account_id, scope);
