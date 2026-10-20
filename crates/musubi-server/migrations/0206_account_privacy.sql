-- Per-account privacy toggles (LootLocker public/private profile parity).
create table if not exists musubi.account_privacy (
    account_id  uuid primary key references musubi.accounts(id) on delete cascade,
    profile     text not null default 'public' check (profile in ('public', 'friends', 'private')),
    inventory   text not null default 'public' check (inventory in ('public', 'friends', 'private')),
    show_online boolean not null default true,
    updated_at  timestamptz not null default now()
);
