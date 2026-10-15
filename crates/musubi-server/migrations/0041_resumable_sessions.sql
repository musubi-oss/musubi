-- Resumable realtime sessions, so a client can continue on any node after its node died or drained.
create table if not exists musubi.resumable_sessions (
    token_hash    text primary key,
    session_id    uuid not null,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    username      text not null,
    status        text,
    node_id       uuid not null,
    cluster_group text not null default '',
    channels      jsonb not null default '[]'::jsonb,
    matches       jsonb not null default '[]'::jsonb,
    follows       jsonb not null default '[]'::jsonb,
    -- true while the session has no transport; a live session's token must not be reusable
    parked        boolean not null default false,
    expires_at    timestamptz not null,
    updated_at    timestamptz not null default now()
);
create index if not exists resumable_sessions_expires_idx on musubi.resumable_sessions (expires_at);
