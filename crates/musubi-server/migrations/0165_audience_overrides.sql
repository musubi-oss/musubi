-- P0-19: identity include/exclude overrides and managed (imported) audiences
alter table musubi.audiences
    add column if not exists kind text not null default 'rule',
    add column if not exists source text;

create table if not exists musubi.audience_overrides (
    audience    text not null references musubi.audiences(name) on delete cascade,
    account_id  uuid not null,
    mode        text not null check (mode in ('include', 'exclude')),
    note        text not null default '',
    expires_at  timestamptz,
    created_at  timestamptz not null default now(),
    primary key (audience, account_id)
);
create index if not exists audience_overrides_account on musubi.audience_overrides (account_id);

create table if not exists musubi.audience_managed_members (
    audience    text not null references musubi.audiences(name) on delete cascade,
    account_id  uuid not null,
    imported_at timestamptz not null default now(),
    primary key (audience, account_id)
);
create index if not exists audience_managed_members_account on musubi.audience_managed_members (account_id);
