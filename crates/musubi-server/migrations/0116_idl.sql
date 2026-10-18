-- plan 3.8: the deployed IDL (one schema for types, storage collections and RPCs); a single row per cluster
create table if not exists musubi.idl (
    name text primary key,
    source text not null,
    updated_at timestamptz not null default now()
);
