-- Registered S3-compatible endpoints (Edgegap endpoint storage parity): operator-managed buckets
-- with sealed credentials and a probe history.
create table if not exists musubi.storage_endpoints (
    name          text primary key,
    endpoint      text not null,
    region        text not null default '',
    bucket        text not null,
    prefix        text not null default '',
    path_style    boolean not null default true,
    access_key    text not null,
    secret_key    text not null,
    note          text not null default '',
    labels        text[] not null default '{}',
    last_test_at  timestamptz,
    last_status   text,
    last_error    text,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);
