-- Legal agreements (competitive P0-49): versioned ToS / privacy / EULA / marketing documents per
-- locale and country, per-account consent records, and the consent gate.
create table if not exists musubi.legal_documents (
    key          text not null,
    version      integer not null,
    locale       text not null default 'en',
    title        text not null,
    url          text not null default '',
    body         text not null default '',
    mandatory    boolean not null default true,
    countries    text[] not null default '{}',
    grace_secs   bigint not null default 0,
    published    boolean not null default false,
    published_at timestamptz,
    retired_at   timestamptz,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now(),
    primary key (key, version, locale)
);

create table if not exists musubi.legal_consents (
    account_id uuid not null references musubi.accounts(id) on delete cascade,
    key        text not null,
    version    integer not null,
    accepted   boolean not null,
    locale     text not null default '',
    country    text not null default '',
    ip         text not null default '',
    user_agent text not null default '',
    at         timestamptz not null default now(),
    primary key (account_id, key, version)
);
create index if not exists legal_consents_key_idx on musubi.legal_consents (key, version, at desc);

create table if not exists musubi.legal_profiles (
    account_id uuid primary key references musubi.accounts(id) on delete cascade,
    country    text not null default '',
    locale     text not null default '',
    updated_at timestamptz not null default now()
);
