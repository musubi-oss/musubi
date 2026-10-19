-- Generic OAuth2 / OIDC provider catalog (authorization-code + PKCE) and in-flight login flows.
create table if not exists musubi.auth_providers (
    name            text primary key,
    kind            text not null check (kind in ('oidc', 'oauth2')),
    display_name    text not null default '',
    enabled         boolean not null default true,
    issuer          text not null default '',
    authorize_url   text not null default '',
    token_url       text not null default '',
    userinfo_url    text not null default '',
    jwks_url        text not null default '',
    client_id       text not null,
    client_secret   text not null default '',
    auth_style      text not null default 'body' check (auth_style in ('body', 'basic')),
    scopes          text[] not null default '{}',
    pkce            boolean not null default true,
    claims          jsonb not null default '{}'::jsonb,
    allow_signup    boolean not null default true,
    redirect_urls   text[] not null default '{}',
    return_urls     text[] not null default '{}',
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);

create table if not exists musubi.auth_flows (
    state           text primary key,
    provider        text not null,
    verifier        text not null,
    nonce           text not null,
    callback_url    text not null,
    return_url      text not null default '',
    account_id      uuid,
    opts            jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    expires_at      timestamptz not null
);
create index if not exists auth_flows_expires_idx on musubi.auth_flows (expires_at);
