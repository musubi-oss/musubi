create table if not exists musubi.build_artifacts (
    name       text not null,
    version    text not null,
    filename   text not null,
    size       bigint not null,
    sha256     text not null,
    mime       text not null default 'application/octet-stream',
    blob       bytea,
    blob_key   text,
    token      text not null,
    notes      text,
    metadata   jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    primary key (name, version)
);
