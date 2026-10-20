-- Session baggage (Pragma parity): per-session key/value entries set by the client or the
-- server, keyed by the refresh family so they survive token rotation. Scopes: `social` (friends
-- see it), `game` (game servers read it), `service` (server-set only; stamped onto the account's
-- bus events for logs/metrics).
create table if not exists musubi.session_baggage (
    family      uuid not null,
    scope       text not null check (scope in ('social', 'game', 'service')),
    key         text not null,
    value       jsonb not null,
    set_by      text not null default 'client',
    updated_at  timestamptz not null default now(),
    primary key (family, scope, key)
);
