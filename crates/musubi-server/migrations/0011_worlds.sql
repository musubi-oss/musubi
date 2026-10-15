-- World directory: which node is home to a meshing world (v0: one replication layer per world).
create table if not exists musubi.worlds (
    name        text primary key,
    node_id     uuid not null,
    config      jsonb not null default '{}'::jsonb,
    controller  jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);
