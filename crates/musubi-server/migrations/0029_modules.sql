-- WASM modules uploaded through the admin API: every node loads them at startup and on
-- `runtime.reload` bus messages, so server logic deploys without restarts.
create table if not exists musubi.modules (
    name        text primary key,
    bytes       bytea not null,
    sha256      text not null,
    size        integer not null,
    updated_at  timestamptz not null default now()
);
