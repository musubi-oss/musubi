create table if not exists musubi.config_sources (
    name            text primary key,
    url             text not null,
    format          text not null check (format in ('csv', 'json')),
    key             text,
    note            text,
    interval_secs   integer not null default 300,
    enabled         boolean not null default true,
    headers         jsonb not null default '{}'::jsonb,
    etag            text,
    last_hash       text,
    last_sync_at    timestamptz,
    last_status     text,
    last_error      text,
    last_changed_at timestamptz,
    syncs           integer not null default 0,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);
