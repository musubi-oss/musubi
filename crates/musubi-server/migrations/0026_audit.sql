-- Who did what through the admin and server-to-server APIs.
create table if not exists musubi.audit_log (
    id          uuid primary key,
    at          timestamptz not null default now(),
    actor       text not null,
    method      text not null,
    route       text not null,
    path        text not null,
    status      integer not null,
    ip          text not null default '',
    body        jsonb,
    duration_ms integer not null default 0
);
create index if not exists audit_log_actor_idx on musubi.audit_log (actor, id desc);
create index if not exists audit_log_route_idx on musubi.audit_log (route, id desc);
