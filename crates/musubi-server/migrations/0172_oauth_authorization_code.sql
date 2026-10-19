-- OAuth2 authorization-code flow (RFC 6749 §4.1 + PKCE RFC 7636): third-party apps act for a player
create table if not exists musubi.oauth_codes (
    code_hash             text primary key,
    client_id             text not null references musubi.oauth_clients(client_id) on delete cascade,
    account_id            uuid not null,
    scopes                text[] not null,
    redirect_uri          text not null,
    code_challenge        text,
    code_challenge_method text,
    state                 text,
    created_at            timestamptz not null default now(),
    expires_at            timestamptz not null,
    consumed_at           timestamptz
);
create index if not exists oauth_codes_expiry_idx on musubi.oauth_codes (expires_at);

create table if not exists musubi.oauth_grants (
    client_id    text not null references musubi.oauth_clients(client_id) on delete cascade,
    account_id   uuid not null,
    scopes       text[] not null,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    last_used_at timestamptz,
    revoked_at   timestamptz,
    primary key (client_id, account_id)
);
create index if not exists oauth_grants_account_idx on musubi.oauth_grants (account_id, updated_at desc);

create table if not exists musubi.oauth_refresh_tokens (
    token_hash   text primary key,
    family       uuid not null,
    client_id    text not null references musubi.oauth_clients(client_id) on delete cascade,
    account_id   uuid not null,
    scopes       text[] not null,
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    last_used_at timestamptz,
    rotated_at   timestamptz,
    revoked_at   timestamptz
);
create index if not exists oauth_refresh_family_idx on musubi.oauth_refresh_tokens (family);
create index if not exists oauth_refresh_grant_idx on musubi.oauth_refresh_tokens (client_id, account_id) where revoked_at is null;
