-- Product analytics (competitive P0-26 / P0-22 / P0-35): client telemetry ingest with a schema
-- registry, the analytics event table KPIs and exports read from.
create table if not exists musubi.analytics_schemas (
    name          text primary key,
    description   text not null default '',
    enabled       boolean not null default true,
    required      text[] not null default '{}',
    sampling_pct  integer not null default 100,
    updated_at    timestamptz not null default now()
);

create table if not exists musubi.analytics_events (
    id          uuid primary key,
    at          timestamptz not null,
    received_at timestamptz not null default now(),
    name        text not null,
    account_id  uuid,
    device_id   text,
    session_id  text,
    platform    text,
    country     text,
    properties  jsonb not null default '{}'::jsonb,
    source      text not null default 'client'
);
create index if not exists analytics_events_at_idx on musubi.analytics_events (at);
create index if not exists analytics_events_name_at_idx on musubi.analytics_events (name, at);
create index if not exists analytics_events_account_idx on musubi.analytics_events (account_id, at) where account_id is not null;
