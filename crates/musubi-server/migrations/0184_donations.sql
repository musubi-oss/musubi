-- Donations: a player asks friends / group mates / anyone for help; contributors are rewarded at
-- once, the recipient claims the accrued reward. Definitions are snapshotted into live donations.
create table if not exists musubi.donation_defs (
    id                          text primary key,
    name                        text not null default '',
    description                 text not null default '',
    cost                        jsonb not null default '{}'::jsonb,
    count                       int not null,
    max_count                   int not null,
    duration_secs               bigint not null,
    user_contribution_max_count int not null default 1,
    recipient_reward            text,
    contributor_reward          text,
    audience                    text not null default 'friends',
    enabled                     boolean not null default true,
    metadata                    jsonb not null default '{}'::jsonb,
    updated_at                  timestamptz not null default now()
);

create table if not exists musubi.donations (
    id                 uuid primary key,
    account_id         uuid not null references musubi.accounts(id) on delete cascade,
    def_id             text not null,
    count              int not null default 0,
    claimed_count      int not null default 0,
    target_count       int not null,
    max_count          int not null,
    user_max           int not null,
    recipient_reward   text,
    contributor_reward text,
    audience           text not null,
    expires_at         timestamptz not null,
    created_at         timestamptz not null default now()
);
create index if not exists donations_account_idx on musubi.donations (account_id, def_id);
create index if not exists donations_expires_idx on musubi.donations (expires_at);

create table if not exists musubi.donation_contributions (
    donation_id uuid not null references musubi.donations(id) on delete cascade,
    account_id  uuid not null,
    count       int not null default 0,
    updated_at  timestamptz not null default now(),
    primary key (donation_id, account_id)
);
