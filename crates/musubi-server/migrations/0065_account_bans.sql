-- Account bans with a reason, a player-visible message, an expiry (auto-lifted by the leader
-- sweep) and a shadow mode (plan 5.2): shadow-banned accounts stay logged in but their chat
-- reaches nobody and their matchmaking tickets never match.
create table if not exists musubi.account_bans (
    account_id uuid primary key references musubi.accounts(id) on delete cascade,
    reason     text,
    message    text,
    until      timestamptz,
    shadow     boolean not null default false,
    actor      text,
    created_at timestamptz not null default now()
);
create index if not exists account_bans_until_idx on musubi.account_bans (until) where until is not null;
