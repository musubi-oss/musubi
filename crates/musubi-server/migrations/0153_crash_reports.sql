-- Client crash / error reports grouped by fingerprint (competitive P1-39) — triage state per
-- group, raw reports with optional minidumps; per-player event history reads musubi.events.
create table if not exists musubi.crash_groups (
    fingerprint          text primary key,
    kind                 text not null,
    title                text not null,
    status               text not null default 'open',
    count                bigint not null default 0,
    first_seen           timestamptz not null default now(),
    last_seen            timestamptz not null default now(),
    first_version        text,
    last_version         text,
    resolved_in_version  text,
    resolved_at          timestamptz,
    assignee             text not null default '',
    notes                text not null default '',
    sample_id            uuid,
    updated_at           timestamptz not null default now()
);
create index if not exists crash_groups_status_last_idx on musubi.crash_groups (status, last_seen desc);

create table if not exists musubi.crash_reports (
    id            uuid primary key,
    fingerprint   text not null references musubi.crash_groups(fingerprint) on delete cascade,
    kind          text not null,
    account_id    uuid,
    device_id     text,
    session_id    text,
    platform      text not null default '',
    app_version   text not null default '',
    build         text not null default '',
    os            text not null default '',
    device        text not null default '',
    locale        text not null default '',
    message       text not null,
    stack         text not null default '',
    breadcrumbs   jsonb not null default '[]'::jsonb,
    tags          jsonb not null default '{}'::jsonb,
    custom        jsonb not null default '{}'::jsonb,
    minidump      bytea,
    duplicates    integer not null default 0,
    at            timestamptz not null,
    received_at   timestamptz not null default now()
);
create index if not exists crash_reports_group_at_idx on musubi.crash_reports (fingerprint, at desc);
create index if not exists crash_reports_account_idx on musubi.crash_reports (account_id, at desc) where account_id is not null;
create index if not exists crash_reports_at_idx on musubi.crash_reports (at);
