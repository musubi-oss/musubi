create table if not exists musubi.platform_friends (
    account_id      uuid not null references musubi.accounts(id) on delete cascade,
    provider        text not null,
    subject         text not null,
    auto_add        boolean not null default false,
    matched_account uuid,
    created_at      timestamptz not null default now(),
    matched_at      timestamptz,
    primary key (account_id, provider, subject)
);
create index if not exists platform_friends_unmatched
    on musubi.platform_friends (provider, subject) where matched_account is null;
