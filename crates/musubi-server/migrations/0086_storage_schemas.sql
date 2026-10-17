alter table musubi.storage_objects add column if not exists schema_version integer not null default 0;

create table if not exists musubi.storage_schemas (
    collection  text primary key,
    version     integer not null default 0,
    schema      jsonb not null default 'null'::jsonb,
    migrations  jsonb not null default '[]'::jsonb,
    indexes     jsonb not null default '[]'::jsonb,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
