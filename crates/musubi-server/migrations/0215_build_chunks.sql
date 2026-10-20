-- Delta build updates: artifacts are stored as content-defined chunks shared across versions;
-- uploaders send a manifest first and only the chunks the server lacks, launchers fetch only the
-- chunks their local cache lacks.
create table if not exists musubi.build_chunks (
    hash       text primary key,
    size       integer not null,
    blob       bytea,
    blob_key   text,
    refs       integer not null default 0,
    created_at timestamptz not null default now()
);
alter table musubi.build_artifacts add column if not exists manifest jsonb;
alter table musubi.build_artifacts add column if not exists delta jsonb;
create table if not exists musubi.build_uploads (
    name        text not null,
    version     text not null,
    filename    text not null,
    mime        text not null default 'application/octet-stream',
    notes       text,
    metadata    jsonb not null default '{}'::jsonb,
    manifest    jsonb not null,
    max_new_pct double precision,
    created_at  timestamptz not null default now(),
    primary key (name, version)
);
