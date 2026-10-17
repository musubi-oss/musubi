create table if not exists musubi.module_versions (
    id              uuid primary key,
    name            text not null,
    version         integer not null,
    bytes           bytea not null,
    sha256          text not null,
    size            integer not null,
    state           text not null default 'active',
    canary_percent  integer not null default 0,
    canary_accounts uuid[] not null default '{}',
    note            text not null default '',
    created_at      timestamptz not null default now(),
    unique (name, version)
);
create index if not exists module_versions_name on musubi.module_versions (name, created_at desc);

create table if not exists musubi.http_recordings (
    id          uuid primary key,
    method      text not null,
    path        text not null,
    headers     jsonb not null default '{}'::jsonb,
    body        text,
    status      integer not null,
    response    text,
    account_id  uuid,
    server_key  boolean not null default false,
    duration_ms integer not null default 0,
    created_at  timestamptz not null default now()
);
create index if not exists http_recordings_path on musubi.http_recordings (path, created_at desc);

create table if not exists musubi.fixtures (
    name        text primary key,
    manifest    jsonb not null,
    accounts    jsonb not null default '[]'::jsonb,
    applied_at  timestamptz not null default now()
);
