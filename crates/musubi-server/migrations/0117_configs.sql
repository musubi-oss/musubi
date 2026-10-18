-- plan 3.8: typed game configs — JSON documents the server writes and clients read, validated
-- against the `[configs.*]` section of the deployed IDL
create table if not exists musubi.configs (
    name text primary key,
    value jsonb not null,
    version integer not null default 1,
    updated_at timestamptz not null default now()
);
