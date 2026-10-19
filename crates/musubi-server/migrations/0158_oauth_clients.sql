-- P1-23: OAuth2 clients / app registrations (client credentials → scoped short-lived server keys).
create table if not exists musubi.oauth_clients (
    client_id       text primary key,
    name            text not null,
    description     text not null default '',
    kind            text not null default 'confidential' check (kind in ('confidential', 'public')),
    role            text not null default 'server',
    scopes          text[] not null default '{}',
    redirect_uris   text[] not null default '{}',
    allowed_ips     text[] not null default '{}',
    token_ttl_secs  bigint not null default 3600,
    enabled         boolean not null default true,
    owner           text,
    metadata        jsonb not null default '{}'::jsonb,
    secrets         jsonb not null default '[]'::jsonb,
    tokens_issued   bigint not null default 0,
    failures        bigint not null default 0,
    last_token_at   timestamptz,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);
create index if not exists server_keys_subject_idx on musubi.server_keys (subject) where revoked_at is null;
