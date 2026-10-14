-- Musubi base schema. Every subsystem owns its own schema namespace; `musubi` holds
-- cross-cutting tables only.
create schema if not exists musubi;

create table if not exists musubi.accounts (
    id            uuid primary key,
    username      text not null,
    display_name  text,
    avatar_url    text,
    lang          text not null default 'en',
    timezone      text,
    location      text,
    metadata      jsonb not null default '{}'::jsonb,
    flags         integer not null default 0,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    disabled_at   timestamptz,
    deleted_at    timestamptz
);
create unique index if not exists accounts_username_key on musubi.accounts (lower(username)) where deleted_at is null;

-- One account, many identities (device, email, steam, ...).
create table if not exists musubi.identities (
    id            uuid primary key,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    provider      text not null,
    subject       text not null,
    verified      boolean not null default false,
    secret_hash   text,
    metadata      jsonb not null default '{}'::jsonb,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    unique (provider, subject)
);
create index if not exists identities_account_idx on musubi.identities (account_id);

-- Refresh-token families; access tokens are stateless JWTs.
create table if not exists musubi.sessions (
    id            uuid primary key,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    family        uuid not null,
    refresh_hash  bytea not null,
    device_id     text,
    user_agent    text,
    ip            inet,
    vars          jsonb not null default '{}'::jsonb,
    created_at    timestamptz not null default now(),
    last_seen_at  timestamptz not null default now(),
    expires_at    timestamptz not null,
    revoked_at    timestamptz
);
create index if not exists sessions_account_idx on musubi.sessions (account_id) where revoked_at is null;
create unique index if not exists sessions_refresh_hash_key on musubi.sessions (refresh_hash);

-- Legacy KV storage (Nakama-compatible permission codes) until typed collections land.
create table if not exists musubi.storage_objects (
    collection       text not null,
    key              text not null,
    owner_id         uuid not null default '00000000-0000-0000-0000-000000000000',
    value            jsonb not null,
    version          text not null,
    read_permission  smallint not null default 1,
    write_permission smallint not null default 1,
    created_at       timestamptz not null default now(),
    updated_at       timestamptz not null default now(),
    primary key (collection, key, owner_id)
);
create index if not exists storage_objects_owner_idx on musubi.storage_objects (owner_id, collection);
