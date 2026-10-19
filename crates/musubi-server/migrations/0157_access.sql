-- P1-25: limited access mode — login allow-list, maintenance, scheduled playtest windows.
create table if not exists musubi.access_policy (
    id          integer primary key check (id = 1),
    policy      jsonb not null default '{}'::jsonb,
    updated_at  timestamptz not null default now()
);
create table if not exists musubi.access_allowlist (
    key         text primary key,
    kind        text not null check (kind in ('account', 'identity', 'email_domain', 'audience')),
    value       text not null,
    note        text,
    added_by    text,
    expires_at  timestamptz,
    created_at  timestamptz not null default now()
);
create index if not exists access_allowlist_kind_idx on musubi.access_allowlist (kind, created_at desc);
create table if not exists musubi.access_denials (
    id          uuid primary key,
    account_id  uuid,
    provider    text not null default '',
    ip          text not null default '',
    mode        text not null,
    reason      text not null,
    created     boolean not null default false,
    created_at  timestamptz not null default now()
);
create index if not exists access_denials_at_idx on musubi.access_denials (created_at desc);
create index if not exists access_denials_account_idx on musubi.access_denials (account_id, created_at desc);
